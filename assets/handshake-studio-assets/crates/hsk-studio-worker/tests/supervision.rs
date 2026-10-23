//! Supervision behaviour against the `hsk-worker-testbin` helper (feature `test-faults`).
use hsk_studio_accord::CancellationToken;
use hsk_studio_worker::{
    Classified, Failure, ReadFailure, RespawnGate, SpawnError, StartRequest, Supervisor,
    SupervisorConfig,
};
use std::{io::Read, path::PathBuf, process::ChildStdout, time::{Duration, Instant}};
use hsk_studio_worker::{ExactLaunch, OneShotEvent, OneShotReason, OneShotRequest,
    OneShotStartError, OneShotTerminal, OutputBounds, OutputStream};
use std::{collections::BTreeMap, ffi::OsString, num::NonZeroUsize};

fn read_reply(stdout: &mut ChildStdout, cap: u32) -> Result<Vec<u8>, ReadFailure<()>> {
    let mut prefix = [0_u8; 4];
    stdout.read_exact(&mut prefix).map_err(|_| ReadFailure::Closed)?;
    let length = u32::from_le_bytes(prefix);
    if length > cap {
        return Err(ReadFailure::Failed(()));
    }
    let mut payload = vec![0_u8; usize::try_from(length).map_err(|_| ReadFailure::Failed(()))?];
    stdout.read_exact(&mut payload).map_err(|_| ReadFailure::Closed)?;
    Ok(payload)
}

fn framed(payload: &[u8]) -> Vec<u8> {
    let mut frame = u32::try_from(payload.len()).unwrap_or(u32::MAX).to_le_bytes().to_vec();
    frame.extend_from_slice(payload);
    frame
}

fn supervisor(mode: &str, max_workers: usize) -> Supervisor {
    Supervisor::new(SupervisorConfig {
        program: PathBuf::from(env!("CARGO_BIN_EXE_hsk-worker-testbin")),
        args: vec![mode.to_owned()],
        env: Vec::new(),
        max_workers,
        max_in_flight_bytes: 1 << 20,
        spawn_timeout: Duration::from_secs(10),
        os_limits: None,
        reply_buffer: None,
        io_thread_name: "hsk-worker-test-io".to_owned(),
    })
}

fn request() -> StartRequest<'static> {
    StartRequest { owner_session: "test", extra_args: &[], keepalive: None, hello_cap: 64 }
}

#[test]
fn echo_round_trip_then_drop_reaps_and_releases_the_permit() {
    let sup = supervisor("echo", 1);
    let cancel = CancellationToken::default();
    let permit = sup.admit(16).expect("admitted");
    assert!(sup.admit(16).is_err(), "the worker budget is spent");
    let (mut session, hello) = sup.spawn(permit, request(), read_reply, &cancel).expect("spawn");
    assert_eq!(hello, b"HI");
    let reply = session.exchange(framed(b"ping"), 64, Duration::from_secs(5), &cancel).expect("reply");
    assert_eq!(reply, b"ping");
    assert!(session.is_alive());
    drop(session);
    let ledger = sup.ledger();
    assert_eq!(ledger.len(), 1);
    assert!(ledger[0].reaped);
    assert_eq!(sup.stats().workers_live, 0);
    assert!(sup.admit(16).is_ok(), "the permit was released after the reap");
}

#[test]
fn deadline_kills_and_reaps_the_hung_worker() {
    let sup = supervisor("hang", 1);
    let cancel = CancellationToken::default();
    let permit = sup.admit(0).expect("admitted");
    let (mut session, _) = sup.spawn(permit, request(), read_reply, &cancel).expect("spawn");
    let started = Instant::now();
    let failure = session
        .exchange(framed(b"x"), 64, Duration::from_millis(100), &cancel)
        .expect_err("deadline");
    assert_eq!(failure, Failure::Deadline);
    let (classified, done) = session.fail(failure);
    assert_eq!(classified, Classified::Deadline);
    assert!(done.killed && done.reaped);
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[test]
fn crash_is_classified_with_the_exit_code() {
    let sup = supervisor("crash7", 1);
    let cancel = CancellationToken::default();
    let permit = sup.admit(0).expect("admitted");
    let (mut session, _) = sup.spawn(permit, request(), read_reply, &cancel).expect("spawn");
    let failure = session
        .exchange(framed(b"x"), 64, Duration::from_secs(10), &cancel)
        .expect_err("crash");
    assert_eq!(failure, Failure::Read(ReadFailure::Closed));
    let (classified, _) = session.fail(failure);
    assert_eq!(classified, Classified::Crashed { exit_code: Some(7) });
    assert_eq!(sup.stats().crashed, 1);
}

#[test]
fn cancel_ends_a_stalled_hello_wait_quickly() {
    let sup = supervisor("silent", 1);
    let cancel = CancellationToken::default();
    let flag = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        flag.cancel();
    });
    let permit = sup.admit(0).expect("admitted");
    let started = Instant::now();
    let error = sup.spawn(permit, request(), read_reply, &cancel).map(|_| ()).expect_err("canceled");
    canceller.join().expect("canceller");
    assert!(matches!(error, SpawnError::Handshake(Classified::Canceled, done) if done.reaped));
    assert!(started.elapsed() < Duration::from_secs(5), "the 10 s spawn timeout was not waited");
    assert!(sup.ledger().iter().all(|entry| entry.reaped));
}

#[test]
fn respawn_gate_bounds_the_rate_and_a_dead_program_is_a_handshake_crash() {
    let gate = RespawnGate::new(2, Duration::from_secs(60));
    assert!(gate.admit() && gate.admit());
    assert!(!gate.admit(), "third start inside the window is refused");

    let sup = supervisor("exit9", 1);
    let permit = sup.admit(0).expect("admitted");
    let error = sup
        .spawn(permit, request(), read_reply, &CancellationToken::default())
        .map(|_| ())
        .expect_err("no HELLO");
    assert!(matches!(
        error,
        SpawnError::Handshake(Classified::Crashed { exit_code: Some(9) }, _)
    ));
}

fn raw_session(sup: &Supervisor, mode: &str, cancel: CancellationToken, deadline: Instant,
    stdout_bytes: u64, stderr_bytes: u64) -> hsk_studio_worker::OneShotSession {
    let program = PathBuf::from(env!("CARGO_BIN_EXE_hsk-worker-testbin"));
    let cwd = program.parent().expect("helper parent");
    let args = [OsString::from(mode)];
    let env = BTreeMap::new();
    sup.start_one_shot(sup.admit(4096).expect("admission"), OneShotRequest {
        owner_session: "raw-test",
        launch: ExactLaunch { executable: &program, cwd, args: &args, env: &env },
        bounds: OutputBounds { chunk_bytes: NonZeroUsize::new(128).unwrap(), stdout_bytes, stderr_bytes },
        keepalive: None,
    }, cancel, deadline).expect("raw start")
}

fn collect_raw(session: &mut hsk_studio_worker::OneShotSession)
    -> (Vec<u8>, Vec<u8>, OneShotTerminal) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    for event in session.by_ref() {
        match event {
            OneShotEvent::Output { stream: OutputStream::Stdout, bytes } => stdout.extend(bytes),
            OneShotEvent::Output { stream: OutputStream::Stderr, bytes } => stderr.extend(bytes),
            OneShotEvent::Terminal(done) => {
                assert!(session.next().is_none(), "terminal iterator is fused");
                return (stdout, stderr, done);
            }
        }
    }
    panic!("missing terminal outcome");
}

#[test]
fn one_shot_separates_raw_streams_without_hello_and_reaps() {
    let sup = supervisor("unused-configured-argument", 1);
    let mut session = raw_session(&sup, "raw", CancellationToken::default(),
        Instant::now() + Duration::from_secs(10), 4096, 4096);
    let (stdout, stderr, done) = collect_raw(&mut session);
    assert_eq!(stdout, b"stdout-only");
    assert_eq!(stderr, b"stderr-only");
    assert_eq!(done.reason, OneShotReason::Exited { success: true, code: Some(0) });
    assert!(done.cleanup.reaped);
    drop(session);
    assert_eq!(sup.stats().workers_live, 0);
    assert!(sup.ledger().iter().all(|entry| entry.reaped));
}

#[test]
fn one_shot_preserves_native_arguments_environment_and_cwd() {
    let sup = supervisor("ignored", 1);
    let program = PathBuf::from(env!("CARGO_BIN_EXE_hsk-worker-testbin"));
    let cwd = program.parent().unwrap();
    let inherited_key = std::env::vars_os().map(|(key, _)| key)
        .find(|key| key != "HSK_ONESHOT_VALUE" && key != "HSK_ONESHOT_EXPECT_CWD")
        .expect("test runner has an inherited environment entry");
    let args = [OsString::from("raw-native"), OsString::from("argument with spaces"), inherited_key];
    let env = BTreeMap::from([
        (OsString::from("HSK_ONESHOT_VALUE"), OsString::from("exact-value")),
        (OsString::from("HSK_ONESHOT_EXPECT_CWD"), cwd.as_os_str().to_owned()),
    ]);
    let mut session = sup.start_one_shot(sup.admit(4096).unwrap(), OneShotRequest {
        owner_session: "native-test",
        launch: ExactLaunch { executable: &program, cwd, args: &args, env: &env },
        bounds: OutputBounds { chunk_bytes: NonZeroUsize::new(128).unwrap(), stdout_bytes: 4096, stderr_bytes: 4096 },
        keepalive: None,
    }, CancellationToken::default(), Instant::now() + Duration::from_secs(10)).expect("native start");
    let (stdout, _, done) = collect_raw(&mut session);
    assert_eq!(stdout, b"native-ok");
    assert_eq!(done.reason, OneShotReason::Exited { success: true, code: Some(0) });
    assert!(done.cleanup.reaped);
}

#[test]
fn one_shot_nonzero_is_a_terminal_exit() {
    let sup = supervisor("ignored", 1);
    let mut session = raw_session(&sup, "exit9", CancellationToken::default(),
        Instant::now() + Duration::from_secs(10), 4096, 4096);
    let (_, _, done) = collect_raw(&mut session);
    assert_eq!(done.reason, OneShotReason::Exited { success: false, code: Some(9) });
    assert!(done.cleanup.reaped);
}

#[test]
fn one_shot_cancel_interrupts_continuous_output() {
    let sup = supervisor("ignored", 1);
    let cancel = CancellationToken::default();
    let mut session = raw_session(&sup, "raw-flood", cancel.clone(),
        Instant::now() + Duration::from_secs(10), u64::MAX, 4096);
    assert!(matches!(session.next(), Some(OneShotEvent::Output { .. })));
    cancel.cancel();
    let (_, _, done) = collect_raw(&mut session);
    assert_eq!(done.reason, OneShotReason::Canceled);
    assert!(done.cleanup.reaped);
}

#[test]
fn one_shot_deadline_interrupts_continuous_output() {
    let sup = supervisor("ignored", 1);
    let mut session = raw_session(&sup, "raw-flood", CancellationToken::default(),
        Instant::now() + Duration::from_millis(100), u64::MAX, 4096);
    let (_, _, done) = collect_raw(&mut session);
    assert_eq!(done.reason, OneShotReason::Deadline);
    assert!(done.cleanup.reaped);
}

#[test]
fn one_shot_output_overflow_is_explicit_and_reaps() {
    let sup = supervisor("ignored", 1);
    let mut session = raw_session(&sup, "raw-flood", CancellationToken::default(),
        Instant::now() + Duration::from_secs(10), 16, 4096);
    let (stdout, _, done) = collect_raw(&mut session);
    assert!(stdout.len() <= 16);
    assert_eq!(done.reason, OneShotReason::OutputLimit(OutputStream::Stdout));
    assert!(done.cleanup.reaped);
}

#[test]
fn one_shot_drop_reaps_and_releases_admission() {
    let sup = supervisor("ignored", 1);
    let session = raw_session(&sup, "silent", CancellationToken::default(),
        Instant::now() + Duration::from_secs(10), 4096, 4096);
    assert!(sup.admit(0).is_err());
    drop(session);
    assert_eq!(sup.stats().workers_live, 0);
    assert!(sup.ledger().iter().all(|entry| entry.reaped));
    assert!(sup.admit(0).is_ok());
}

#[test]
fn one_shot_redacts_raw_debug_payloads() {
    let event = OneShotEvent::Output { stream: OutputStream::Stderr, bytes: b"private-prompt".to_vec() };
    assert!(!format!("{event:?}").contains("private-prompt"));
    let program = PathBuf::from("private-executable");
    let args = [OsString::from("private-argument")];
    let env = BTreeMap::new();
    let launch = ExactLaunch { executable: &program, cwd: &program, args: &args, env: &env };
    assert!(!format!("{launch:?}").contains("private"));
}

#[test]
fn one_shot_requested_caps_are_rejected_without_spawn() {
    let mut config = SupervisorConfig {
        program: PathBuf::from(env!("CARGO_BIN_EXE_hsk-worker-testbin")),
        args: Vec::new(), env: Vec::new(), max_workers: 1,
        max_in_flight_bytes: 4096, spawn_timeout: Duration::from_secs(1),
        os_limits: None, reply_buffer: None, io_thread_name: "unused".to_owned(),
    };
    config.os_limits = Some(hsk_studio_worker::OsLimits {
        process_memory_bytes: 1 << 20, cpu_time: Duration::from_secs(1),
    });
    let program = config.program.clone();
    let sup = Supervisor::new(config);
    let args = [OsString::from("raw")];
    let env = BTreeMap::new();
    let result = sup.start_one_shot(sup.admit(0).unwrap(), OneShotRequest {
        owner_session: "caps-test",
        launch: ExactLaunch { executable: &program, cwd: program.parent().unwrap(), args: &args, env: &env },
        bounds: OutputBounds { chunk_bytes: NonZeroUsize::new(128).unwrap(), stdout_bytes: 4096, stderr_bytes: 4096 },
        keepalive: None,
    }, CancellationToken::default(), Instant::now() + Duration::from_secs(10));
    assert!(matches!(result, Err(OneShotStartError::UnsupportedCaps)));
    assert_eq!(sup.stats().spawned, 0);
    assert_eq!(sup.stats().workers_live, 0);
    assert!(sup.ledger().is_empty());
}

#[test]
fn one_shot_rejects_a_permit_from_another_supervisor_without_spawn() {
    let sup = supervisor("ignored", 1);
    let other = supervisor("ignored", 1);
    let program = PathBuf::from(env!("CARGO_BIN_EXE_hsk-worker-testbin"));
    let args = [OsString::from("raw")];
    let env = BTreeMap::new();
    let result = sup.start_one_shot(other.admit(0).unwrap(), OneShotRequest {
        owner_session: "foreign-permit",
        launch: ExactLaunch { executable: &program, cwd: program.parent().unwrap(), args: &args, env: &env },
        bounds: OutputBounds { chunk_bytes: NonZeroUsize::new(128).unwrap(), stdout_bytes: 4096, stderr_bytes: 4096 },
        keepalive: None,
    }, CancellationToken::default(), Instant::now() + Duration::from_secs(10));
    assert!(matches!(result, Err(OneShotStartError::InvalidLaunch)));
    assert_eq!(sup.stats().spawned, 0);
    assert_eq!(other.stats().workers_live, 0);
    assert!(sup.ledger().is_empty());
    assert!(other.ledger().is_empty());
}

#[cfg(windows)]
#[test]
fn one_shot_refuses_windows_shell_scripts_before_spawn() {
    for extension in ["cmd", "bat", "CMD", "BAT"] {
        let sup = supervisor("ignored", 1);
        let program = PathBuf::from(env!("CARGO_BIN_EXE_hsk-worker-testbin")).with_extension(extension);
        let env = BTreeMap::new();
        let result = sup.start_one_shot(sup.admit(0).unwrap(), OneShotRequest {
            owner_session: "shell-refusal",
            launch: ExactLaunch { executable: &program, cwd: program.parent().unwrap(), args: &[], env: &env },
            bounds: OutputBounds { chunk_bytes: NonZeroUsize::new(128).unwrap(), stdout_bytes: 4096, stderr_bytes: 4096 },
            keepalive: None,
        }, CancellationToken::default(), Instant::now() + Duration::from_secs(10));
        assert!(matches!(result, Err(OneShotStartError::InvalidLaunch)));
        assert_eq!(sup.stats().spawned, 0);
        assert_eq!(sup.stats().workers_live, 0);
        assert!(sup.ledger().is_empty());
    }
}

#[cfg(windows)]
#[test]
fn one_shot_refuses_windows_case_equivalent_environment_names() {
    for (left, right) in [("PATH", "Path"), ("HSK_Ä", "HSK_ä")] {
        let sup = supervisor("ignored", 1);
        let program = PathBuf::from(env!("CARGO_BIN_EXE_hsk-worker-testbin"));
        let args = [OsString::from("raw")];
        let env = BTreeMap::from([
            (OsString::from(left), OsString::from("first")),
            (OsString::from(right), OsString::from("second")),
        ]);
        let result = sup.start_one_shot(sup.admit(0).unwrap(), OneShotRequest {
            owner_session: "environment-collision",
            launch: ExactLaunch { executable: &program, cwd: program.parent().unwrap(), args: &args, env: &env },
            bounds: OutputBounds { chunk_bytes: NonZeroUsize::new(128).unwrap(), stdout_bytes: 4096, stderr_bytes: 4096 },
            keepalive: None,
        }, CancellationToken::default(), Instant::now() + Duration::from_secs(10));
        assert!(matches!(result, Err(OneShotStartError::InvalidLaunch)));
        assert_eq!(sup.stats().spawned, 0);
        assert_eq!(sup.stats().workers_live, 0);
        assert!(sup.ledger().is_empty());
    }
}
