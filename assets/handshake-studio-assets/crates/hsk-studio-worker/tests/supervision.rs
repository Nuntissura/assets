//! Supervision behaviour against the `hsk-worker-testbin` helper (feature `test-faults`).
use hsk_studio_accord::CancellationToken;
use hsk_studio_worker::{
    Classified, Failure, ReadFailure, RespawnGate, SpawnError, StartRequest, Supervisor,
    SupervisorConfig,
};
use std::{io::Read, path::PathBuf, process::ChildStdout, time::{Duration, Instant}};

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
