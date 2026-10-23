#![forbid(unsafe_code)]

use fforager_contracts::{
    ErrorCode, FrameDecoder, FrameLimits, JavaScriptWorkerEnvelope, JavaScriptWorkerMessage,
    ProducerId, ProtocolLimits,
};
use fforager_javascript::{
    CacheKey, MAXIMUM_FRAME_BYTES, MAXIMUM_JOBS_PER_WORKER, MAXIMUM_NATIVE_THREAD_STACK_BYTES,
    MAXIMUM_WORKER_AGE_MILLIS, SolverError, WorkerCache, WorkerInput, execute_prepared,
    execute_probe, prepare_player, read_frame, verify_program_hash, write_frame,
};
use std::{
    env, fs,
    io::{self, BufReader, BufWriter},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError},
    thread,
    time::{Duration, Instant},
};

const CORPUS_PRODUCER_ID: &str = "producer_fforager_js_corpus";
const WORKER_PRODUCER_ID: &str = "producer_fforager_js_worker";
const PLAYER_CAPABILITY_GRANT: &str = "capability-none-v1";
const PROBE_GRANT_ENV: &str = "FF_WP006_PROBE_GRANT";

fn main() {
    let worker = thread::Builder::new()
        .name("fforager-js-worker".to_owned())
        .stack_size(MAXIMUM_NATIVE_THREAD_STACK_BYTES)
        .spawn(run);
    let result = match worker {
        Ok(worker) => worker
            .join()
            .unwrap_or_else(|_| Err(SolverError::Evaluation("worker thread panicked".to_owned()))),
        Err(error) => Err(SolverError::Io(error.to_string())),
    };
    if let Err(error) = result {
        eprintln!("FF-JS-WORKER-FATAL:{}:{error}", fatal_category(&error));
        std::process::exit(2);
    }
}

fn run() -> Result<(), SolverError> {
    let player_root = parse_player_root()?;
    let probe_grant = env::var(PROBE_GRANT_ENV)
        .map_err(|_| SolverError::InvalidResult("missing probe capability grant".to_owned()))?;
    if !probe_grant.starts_with("capability-wp006-probe-")
        || probe_grant.len() != "capability-wp006-probe-".len() + 64
    {
        return Err(SolverError::InvalidResult(
            "invalid probe capability grant".to_owned(),
        ));
    }
    let inbound = spawn_reader()?;
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    let started = Instant::now();
    let mut completed_jobs = 0_u64;
    let mut cache = WorkerCache::default();
    let mut last_sequence = 0_u64;

    loop {
        if completed_jobs >= MAXIMUM_JOBS_PER_WORKER
            || elapsed_millis(started) >= MAXIMUM_WORKER_AGE_MILLIS
        {
            return Ok(());
        }
        let Some(envelope) = receive_envelope(&inbound)? else {
            return Ok(());
        };
        validate_next_sequence(&envelope, &mut last_sequence)?;
        if let Err(error) = authorize_envelope(&envelope, &probe_grant) {
            let response = response_envelope(
                envelope,
                failure_message(ErrorCode::InvalidInput, &error.to_string()),
            )?;
            write_envelope(&mut writer, &response)?;
            completed_jobs = completed_jobs.saturating_add(1);
            continue;
        }
        if !matches!(envelope.message, JavaScriptWorkerMessage::Evaluate { .. }) {
            let response = response_envelope(
                envelope,
                failure_message(ErrorCode::InvalidInput, "idle worker accepts evaluate only"),
            )?;
            write_envelope(&mut writer, &response)?;
            completed_jobs = completed_jobs.saturating_add(1);
            continue;
        }
        cache = run_active_job(
            &player_root,
            envelope,
            cache,
            &probe_grant,
            &inbound,
            &mut writer,
            &mut last_sequence,
        )?;
        completed_jobs = completed_jobs.saturating_add(1);
    }
}

fn spawn_reader()
-> Result<Receiver<Result<Option<JavaScriptWorkerEnvelope>, SolverError>>, SolverError> {
    let (sender, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("fforager-js-worker-frame-reader".to_owned())
        .spawn(move || {
            let stdin = io::stdin();
            let mut reader = BufReader::new(stdin.lock());
            let frame_limits = FrameLimits {
                maximum_frame_bytes: MAXIMUM_FRAME_BYTES,
            };
            let protocol_limits = ProtocolLimits {
                maximum_message_bytes: MAXIMUM_FRAME_BYTES,
                ..ProtocolLimits::default()
            };
            loop {
                let result = match read_frame(&mut reader, frame_limits) {
                    Ok(Some(payload)) => FrameDecoder::decode_javascript_worker_with_limits(
                        &payload,
                        frame_limits,
                        protocol_limits,
                    )
                    .map(Some)
                    .map_err(|error| SolverError::Frame(error.to_string())),
                    Ok(None) => {
                        let _ = sender.send(Ok(None));
                        break;
                    }
                    Err(error) => Err(error),
                };
                let terminal = result.is_err();
                if sender.send(result).is_err() || terminal {
                    break;
                }
            }
        })
        .map_err(fforager_javascript::io_error)?;
    Ok(receiver)
}

fn receive_envelope(
    inbound: &Receiver<Result<Option<JavaScriptWorkerEnvelope>, SolverError>>,
) -> Result<Option<JavaScriptWorkerEnvelope>, SolverError> {
    inbound
        .recv()
        .map_err(|_| SolverError::Frame("worker stdin reached EOF".to_owned()))?
}

fn run_active_job(
    player_root: &Path,
    request: JavaScriptWorkerEnvelope,
    cache: WorkerCache,
    probe_grant: &str,
    inbound: &Receiver<Result<Option<JavaScriptWorkerEnvelope>, SolverError>>,
    writer: &mut BufWriter<io::StdoutLock<'_>>,
    last_sequence: &mut u64,
) -> Result<WorkerCache, SolverError> {
    let root = player_root.to_owned();
    let active_request = request.clone();
    let (result_sender, result_receiver) = mpsc::channel();
    thread::Builder::new()
        .name("fforager-js-worker-evaluation".to_owned())
        .stack_size(MAXIMUM_NATIVE_THREAD_STACK_BYTES)
        .spawn(move || {
            let mut cache = cache;
            let response = handle_request(&root, request, &mut cache);
            let _ = result_sender.send((response, cache));
        })
        .map_err(fforager_javascript::io_error)?;

    loop {
        match result_receiver.recv_timeout(Duration::from_millis(10)) {
            Ok((response, cache)) => {
                write_envelope(writer, &response?)?;
                return Ok(cache);
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(SolverError::Evaluation(
                    "evaluation thread disconnected".to_owned(),
                ));
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        match inbound.try_recv() {
            Ok(Ok(Some(cancel))) => {
                validate_next_sequence(&cancel, last_sequence)?;
                authorize_envelope(&cancel, probe_grant)?;
                validate_cancellation(&active_request, &cancel)?;
                let JavaScriptWorkerMessage::Cancelled { generation } = cancel.message else {
                    return Err(SolverError::InvalidResult(
                        "only cancellation control is accepted while a job is active".to_owned(),
                    ));
                };
                let response =
                    response_envelope(cancel, JavaScriptWorkerMessage::Cancelled { generation })?;
                write_envelope(writer, &response)?;
                std::process::exit(0);
            }
            Ok(Ok(None)) => {
                return Err(SolverError::Frame(
                    "worker stdin ended during active job".to_owned(),
                ));
            }
            Ok(Err(error)) => return Err(error),
            Err(TryRecvError::Disconnected) => {
                return Err(SolverError::Frame("worker stdin disconnected".to_owned()));
            }
            Err(TryRecvError::Empty) => {}
        }
    }
}

fn validate_cancellation(
    active: &JavaScriptWorkerEnvelope,
    cancel: &JavaScriptWorkerEnvelope,
) -> Result<(), SolverError> {
    let JavaScriptWorkerMessage::Cancelled { generation } = cancel.message else {
        return Err(SolverError::InvalidResult(
            "active worker requires cancellation control".to_owned(),
        ));
    };
    if generation == 0
        || cancel.header.request_id != active.header.request_id
        || cancel.header.job_id != active.header.job_id
        || cancel.compatibility != active.compatibility
        || cancel.provenance != active.provenance
    {
        return Err(SolverError::InvalidResult(
            "cancellation correlation mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn validate_next_sequence(
    envelope: &JavaScriptWorkerEnvelope,
    last_sequence: &mut u64,
) -> Result<(), SolverError> {
    let expected = last_sequence
        .checked_add(1)
        .ok_or_else(|| SolverError::InvalidResult("request sequence exhausted".to_owned()))?;
    if envelope.header.sequence != expected {
        return Err(SolverError::InvalidResult(format!(
            "request sequence mismatch: expected {expected}, observed {}",
            envelope.header.sequence
        )));
    }
    *last_sequence = expected;
    Ok(())
}

fn authorize_envelope(
    envelope: &JavaScriptWorkerEnvelope,
    probe_grant: &str,
) -> Result<(), SolverError> {
    if envelope.header.producer_id.as_str() != CORPUS_PRODUCER_ID {
        return Err(SolverError::InvalidResult(
            "unauthorized worker producer".to_owned(),
        ));
    }
    let expected_grant = match &envelope.message {
        JavaScriptWorkerMessage::Evaluate { input, .. } => {
            let input: WorkerInput = serde_json::from_value(input.clone())
                .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
            if input.challenge.requires_player() {
                PLAYER_CAPABILITY_GRANT
            } else {
                probe_grant
            }
        }
        JavaScriptWorkerMessage::Cancelled { .. } => {
            envelope.provenance.capability_grant_id.as_str()
        }
        JavaScriptWorkerMessage::Result { .. } | JavaScriptWorkerMessage::Failed { .. } => {
            return Err(SolverError::InvalidResult(
                "worker accepts request/control messages only".to_owned(),
            ));
        }
    };
    if envelope.provenance.capability_grant_id != expected_grant {
        return Err(SolverError::InvalidResult(
            "unauthorized worker capability grant".to_owned(),
        ));
    }
    Ok(())
}

fn parse_player_root() -> Result<PathBuf, SolverError> {
    let mut arguments = env::args_os().skip(1);
    let flag = arguments
        .next()
        .ok_or_else(|| SolverError::InvalidResult("missing --player-root".to_owned()))?;
    if flag != "--player-root" {
        return Err(SolverError::InvalidResult(
            "expected --player-root".to_owned(),
        ));
    }
    let root = arguments
        .next()
        .ok_or_else(|| SolverError::InvalidResult("missing player root path".to_owned()))?;
    if arguments.next().is_some() {
        return Err(SolverError::InvalidResult(
            "unexpected worker arguments".to_owned(),
        ));
    }
    fs::canonicalize(root).map_err(|error| SolverError::Io(error.to_string()))
}

fn handle_request(
    player_root: &Path,
    envelope: JavaScriptWorkerEnvelope,
    cache: &mut WorkerCache,
) -> Result<JavaScriptWorkerEnvelope, SolverError> {
    let result = evaluate_request(player_root, &envelope, cache);
    let message = match result {
        Ok(output) => match serde_json::to_value(output) {
            Ok(output) => JavaScriptWorkerMessage::Result { output },
            Err(error) => failure_message(
                ErrorCode::Internal,
                &format!("result serialization failed: {error}"),
            ),
        },
        Err(error) => failure_message(error_code(&error), &error.to_string()),
    };
    response_envelope(envelope, message)
}

fn evaluate_request(
    player_root: &Path,
    envelope: &JavaScriptWorkerEnvelope,
    cache: &mut WorkerCache,
) -> Result<fforager_javascript::WorkerOutput, SolverError> {
    let JavaScriptWorkerMessage::Evaluate {
        program_reference,
        input,
        ..
    } = &envelope.message
    else {
        return Err(SolverError::InvalidResult(
            "worker accepts evaluate requests only".to_owned(),
        ));
    };
    let input: WorkerInput = serde_json::from_value(input.clone())
        .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
    if !input.challenge.requires_player() {
        return execute_probe(&input);
    }
    let program_path = confined_program_path(player_root, program_reference)?;
    let program = fs::read(&program_path).map_err(|error| SolverError::Io(error.to_string()))?;
    verify_program_hash(&program, &input.script_sha256)?;
    let key = CacheKey::from_input(&input);
    if let Some(prepared) = cache.get(&key) {
        return execute_prepared(prepared, &input, true);
    }
    let prepared = prepare_player(&program)?;
    let output = execute_prepared(&prepared, &input, false)?;
    cache.insert(key, prepared);
    Ok(output)
}

fn confined_program_path(root: &Path, reference: &str) -> Result<PathBuf, SolverError> {
    let requested = root.join(reference);
    let canonical =
        fs::canonicalize(&requested).map_err(|error| SolverError::Io(error.to_string()))?;
    if !canonical.starts_with(root) || !canonical.is_file() {
        return Err(SolverError::InvalidResult(
            "program reference escaped the player root".to_owned(),
        ));
    }
    Ok(canonical)
}

fn response_envelope(
    request: JavaScriptWorkerEnvelope,
    message: JavaScriptWorkerMessage,
) -> Result<JavaScriptWorkerEnvelope, SolverError> {
    let mut header = request.header;
    header.producer_id = ProducerId::new(WORKER_PRODUCER_ID)
        .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
    header.sequence = header
        .sequence
        .checked_add(1)
        .ok_or_else(|| SolverError::InvalidResult("response sequence exhausted".to_owned()))?;
    Ok(JavaScriptWorkerEnvelope {
        header,
        compatibility: request.compatibility,
        provenance: request.provenance,
        message,
    })
}

fn write_envelope(
    writer: &mut BufWriter<io::StdoutLock<'_>>,
    envelope: &JavaScriptWorkerEnvelope,
) -> Result<(), SolverError> {
    let payload = serde_json::to_vec(envelope)
        .map_err(|error| SolverError::InvalidResult(error.to_string()))?;
    write_frame(
        writer,
        &payload,
        FrameLimits {
            maximum_frame_bytes: MAXIMUM_FRAME_BYTES,
        },
    )
}

fn failure_message(error: ErrorCode, message: &str) -> JavaScriptWorkerMessage {
    let mut bounded = message.to_owned();
    bounded.truncate(4096);
    if bounded.is_empty() {
        bounded.push_str("unspecified worker failure");
    }
    JavaScriptWorkerMessage::Failed {
        error,
        message: bounded,
    }
}

fn error_code(error: &SolverError) -> ErrorCode {
    match error {
        SolverError::ProgramTooLarge { .. } | SolverError::OutputTooLarge { .. } => {
            ErrorCode::ResourceExhausted
        }
        SolverError::InvalidProgram(_)
        | SolverError::UnsupportedPlayerStructure
        | SolverError::NoChallengeCandidate
        | SolverError::InvalidCandidate(_)
        | SolverError::ScriptHashMismatch { .. }
        | SolverError::InvalidResult(_) => ErrorCode::InvalidInput,
        SolverError::Evaluation(_) => ErrorCode::DependencyFailed,
        SolverError::Io(_) | SolverError::Frame(_) => ErrorCode::Internal,
    }
}

fn fatal_category(error: &SolverError) -> &'static str {
    let message = error.to_string();
    if message.contains("ZeroLength") {
        "frame_zero"
    } else if message.contains("Oversized") {
        "frame_oversized"
    } else if message.contains("partial header") {
        "frame_partial_header"
    } else if message.contains("partial payload") {
        "frame_partial_payload"
    } else if message.contains("protocol validation failed")
        && (message.contains("IncompatibleVersion") || message.contains("InvalidVersion"))
    {
        "protocol_version"
    } else if message.contains("protocol validation failed")
        && message.contains("NumericLimitExceeded")
    {
        "protocol_numeric_limit"
    } else if message.contains("protocol validation failed")
        && message.contains("InvalidField")
        && message.contains("program_reference")
    {
        "protocol_reference"
    } else if message.contains("unknown field") {
        "protocol_unknown_field"
    } else if message.contains("InvalidJson") {
        "frame_invalid_json"
    } else {
        "internal"
    }
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fforager_contracts::{
        BoundaryProvenance, CompatibilityRange, EnvelopeHeader, JobId, RequestId, SchemaVersion,
    };
    use fforager_javascript::ChallengeKind;

    #[test]
    fn traversal_reference_is_rejected_before_read() {
        let root = Path::new(".");
        let result = confined_program_path(root, "../outside");
        assert!(result.is_err());
    }

    #[test]
    fn destructive_probe_requires_host_grant() {
        let envelope = test_envelope(ChallengeKind::Crash, "capability-attacker");
        assert!(authorize_envelope(&envelope, "capability-wp006-probe-0000000000000000000000000000000000000000000000000000000000000000").is_err());
    }

    #[test]
    fn response_sequence_overflow_fails_closed() {
        let mut envelope = test_envelope(ChallengeKind::N, PLAYER_CAPABILITY_GRANT);
        envelope.header.sequence = u64::MAX;
        assert!(
            response_envelope(
                envelope,
                JavaScriptWorkerMessage::Cancelled { generation: 1 }
            )
            .is_err()
        );
    }

    fn test_envelope(challenge: ChallengeKind, grant: &str) -> JavaScriptWorkerEnvelope {
        JavaScriptWorkerEnvelope {
            header: EnvelopeHeader {
                schema_id: "ff.javascript-worker@1".to_owned(),
                version: SchemaVersion { major: 1, minor: 0 },
                request_id: RequestId::new("request_wp006_test").expect("valid request"),
                producer_id: ProducerId::new(CORPUS_PRODUCER_ID).expect("valid producer"),
                job_id: Some(JobId::new("job_wp006_test").expect("valid job")),
                sequence: 1,
            },
            compatibility: CompatibilityRange {
                major: 1,
                minimum_minor: 0,
                maximum_minor: 1,
            },
            provenance: BoundaryProvenance {
                artifact_id: "artifact-test".to_owned(),
                schema_hash: fforager_javascript::sha256_hex(b"ff.javascript-worker@1"),
                capability_grant_id: grant.to_owned(),
            },
            message: JavaScriptWorkerMessage::Evaluate {
                program_reference: "player.js".to_owned(),
                input: serde_json::to_value(WorkerInput {
                    challenge,
                    value: String::new(),
                    script_sha256: "0".repeat(64),
                    extractor_version: "test-v1".to_owned(),
                })
                .expect("serializable input"),
                deadline_millis: 1_000,
                memory_limit_bytes: 128 * 1024 * 1024,
            },
        }
    }
}
