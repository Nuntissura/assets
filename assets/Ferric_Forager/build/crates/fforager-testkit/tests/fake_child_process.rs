use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const WAIT_LIMIT: Duration = Duration::from_secs(10);
const CAPTURE_LIMIT: usize = 64 * 1_024;
const READ_BUFFER_BYTES: usize = 8 * 1_024;
static CASE_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn fake_child() -> &'static str {
    env!("CARGO_BIN_EXE_fforager-fake-child")
}

#[derive(Debug)]
struct BoundedCapture {
    bytes: Vec<u8>,
    total_bytes: u64,
    truncated: bool,
}

#[derive(Debug)]
struct CapturedOutput {
    status: ExitStatus,
    stdout: BoundedCapture,
    stderr: BoundedCapture,
}

fn drain_bounded(mut pipe: impl Read) -> BoundedCapture {
    let mut buffer = [0_u8; READ_BUFFER_BYTES];
    let mut bytes = Vec::with_capacity(CAPTURE_LIMIT);
    let mut total_bytes = 0_u64;
    loop {
        let read = pipe.read(&mut buffer).expect("fake-child pipe must drain");
        if read == 0 {
            break;
        }
        total_bytes = total_bytes
            .checked_add(u64::try_from(read).expect("read length must fit u64"))
            .expect("bounded fake-child byte count must not overflow");
        let remaining = CAPTURE_LIMIT.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..read.min(remaining)]);
    }
    BoundedCapture {
        truncated: total_bytes > u64::try_from(bytes.len()).expect("capture length must fit u64"),
        bytes,
        total_bytes,
    }
}

fn wait_bounded(mut child: Child) -> CapturedOutput {
    let stdout = child.stdout.take().expect("piped stdout must exist");
    let stderr = child.stderr.take().expect("piped stderr must exist");
    let stdout_drain = thread::spawn(move || drain_bounded(stdout));
    let stderr_drain = thread::spawn(move || drain_bounded(stderr));

    let started = Instant::now();
    let observed_status = loop {
        match child
            .try_wait()
            .expect("fake-child wait must be observable")
        {
            Some(status) => break status,
            None if started.elapsed() < WAIT_LIMIT => thread::sleep(Duration::from_millis(10)),
            None => {
                child
                    .kill()
                    .expect("test may terminate only the fake child it started");
                let status = child.wait().expect("terminated fake child must be reaped");
                let stdout = stdout_drain.join().expect("stdout drain must join");
                let stderr = stderr_drain.join().expect("stderr drain must join");
                panic!(
                    "fake child exceeded {WAIT_LIMIT:?}: status={status:?} stdout_bytes={} stderr_bytes={} stderr_prefix={}",
                    stdout.total_bytes,
                    stderr.total_bytes,
                    String::from_utf8_lossy(&stderr.bytes)
                );
            }
        }
    };
    let status = child
        .wait()
        .expect("observed fake child must be reaped once");
    assert_eq!(status, observed_status);
    CapturedOutput {
        status,
        stdout: stdout_drain.join().expect("stdout drain must join"),
        stderr: stderr_drain.join().expect("stderr drain must join"),
    }
}

fn spawn_piped(arguments: &[&str]) -> Child {
    Command::new(fake_child())
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("fake-child binary must spawn directly without a shell")
}

fn artifact_case(name: &str) -> PathBuf {
    let current = std::env::current_dir().expect("test working directory must resolve");
    let root = current
        .ancestors()
        .find(|candidate| candidate.join("START_HERE.yaml").is_file())
        .expect("tests must execute inside the Ferric repository");
    let sequence = CASE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let directory = root
        .join(".fforager-artifacts")
        .join("test-runs")
        .join(format!(
            "fforager-testkit-{}-{sequence}-{name}",
            std::process::id()
        ));
    fs::create_dir_all(&directory).expect("artifact-contained test directory must be created");
    directory
}

fn wait_until_absent(path: &Path, limit: Duration) {
    let started = Instant::now();
    while path.exists() && started.elapsed() < limit {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !path.exists(),
        "bounded descendant marker remained: {}",
        path.display()
    );
}

fn wait_for_direct_exit(child: &mut Child, limit: Duration) -> ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .expect("direct-child status must be observable")
        {
            return status;
        }
        assert!(
            started.elapsed() < limit,
            "direct child did not exit within {limit:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(windows)]
fn observe_cleanup_exit(child: &mut Child) -> std::io::Result<Option<ExitStatus>> {
    let started = Instant::now();
    loop {
        let status = child.try_wait()?;
        if status.is_some() || started.elapsed() >= WAIT_LIMIT {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(windows)]
fn descendant_timeout_diagnostics(child: &mut Child, marker: &Path) -> String {
    use std::os::windows::process::CommandExt;

    let pid = child.id();
    let status_before = child.try_wait();
    let marker_before = fs::metadata(marker).map(|metadata| metadata.len());
    let stdout = child
        .stdout
        .take()
        .map(|pipe| thread::spawn(move || drain_bounded(pipe)));
    let stderr = child
        .stderr
        .take()
        .map(|pipe| thread::spawn(move || drain_bounded(pipe)));
    // Keep the owned Child handle alive through tree cleanup so its PID cannot recycle.
    let tree_cleanup = match child.try_wait() {
        Ok(None) => std::env::var_os("SystemRoot")
            .ok_or_else(|| "SystemRoot unavailable".to_owned())
            .and_then(|root| {
                let mut cleanup = Command::new(PathBuf::from(root).join("System32/taskkill.exe"))
                    .args(["/PID", &pid.to_string(), "/T", "/F"])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(0x0800_0000)
                    .spawn()
                    .map_err(|error| error.to_string())?;
                let status = observe_cleanup_exit(&mut cleanup);
                if !matches!(status, Ok(Some(_))) {
                    let kill = cleanup.kill();
                    let reap = observe_cleanup_exit(&mut cleanup);
                    return Err(format!(
                        "taskkill incomplete: {status:?}; kill={kill:?}; reap={reap:?}"
                    ));
                }
                status.map_err(|error| error.to_string())
            }),
        state => Err(format!(
            "tree cleanup not issued: owned parent state={state:?}"
        )),
    };
    let direct_kill = match child.try_wait() {
        Ok(None) => Some(child.kill()),
        _ => None,
    };
    let direct_cleanup = observe_cleanup_exit(child);
    let drains_started = Instant::now();
    while (stdout.as_ref().is_some_and(|drain| !drain.is_finished())
        || stderr.as_ref().is_some_and(|drain| !drain.is_finished()))
        && drains_started.elapsed() < WAIT_LIMIT
    {
        thread::sleep(Duration::from_millis(10));
    }
    let stdout = finished_diagnostic_capture(stdout);
    let stderr = finished_diagnostic_capture(stderr);
    format!(
        "pid={pid} status_before={status_before:?} marker_before={marker_before:?} tree_cleanup={tree_cleanup:?} direct_kill={direct_kill:?} direct_cleanup={direct_cleanup:?} marker_after={:?}; descendant cleanup is not inferred from taskkill status; stdout={stdout}; stderr={stderr}",
        fs::metadata(marker).map(|metadata| metadata.len())
    )
}

#[cfg(windows)]
fn finished_diagnostic_capture(drain: Option<thread::JoinHandle<BoundedCapture>>) -> String {
    match drain {
        Some(drain) if drain.is_finished() => match drain.join() {
            Ok(capture) => format!(
                "bytes={} truncated={} prefix={:?}",
                capture.total_bytes,
                capture.truncated,
                String::from_utf8_lossy(&capture.bytes)
            ),
            Err(_) => "capture worker panicked".to_owned(),
        },
        Some(_) => "capture unavailable at cleanup deadline".to_owned(),
        None => "pipe unavailable".to_owned(),
    }
}

#[test]
fn spawned_progress_preserves_stdout_stderr_and_exit_boundaries() {
    let output = wait_bounded(spawn_piped(&[
        "progress-flood",
        "--records",
        "3",
        "--payload-bytes",
        "2",
        "--delay-ms",
        "0",
    ]));
    assert!(output.status.success());
    assert_eq!(output.stderr.total_bytes, 0);
    assert!(!output.stdout.truncated);
    let progress = String::from_utf8(output.stdout.bytes).expect("progress fixture must be UTF-8");
    assert_eq!(progress.matches("progress=continue\n").count(), 2);
    assert_eq!(progress.matches("progress=end\n").count(), 1);
    assert!(progress.ends_with("progress=end\n"));
}

#[test]
fn concurrent_fixed_buffer_drains_release_a_pipe_capacity_block() {
    let mut child = spawn_piped(&[
        "dual-flood",
        "--records",
        "8192",
        "--payload-bytes",
        "2048",
        "--delay-ms",
        "0",
    ]);
    thread::sleep(Duration::from_millis(100));
    assert!(
        child
            .try_wait()
            .expect("blocked child status must be observable")
            .is_none(),
        "the flood must exceed pipe capacity before concurrent drains begin"
    );
    let output = wait_bounded(child);
    assert!(output.status.success());
    assert!(output.stdout.truncated);
    assert!(output.stderr.truncated);
    assert!(output.stdout.total_bytes > u64::try_from(CAPTURE_LIMIT).expect("limit fits u64"));
    assert!(output.stderr.total_bytes > u64::try_from(CAPTURE_LIMIT).expect("limit fits u64"));
    assert_eq!(output.stdout.bytes.len(), CAPTURE_LIMIT);
    assert_eq!(output.stderr.bytes.len(), CAPTURE_LIMIT);
}

#[test]
fn malformed_reordered_truncated_and_stalled_progress_are_distinct() {
    let malformed = wait_bounded(spawn_piped(&[
        "progress-malformed",
        "--kind",
        "invalid-progress",
    ]));
    assert_eq!(malformed.stdout.bytes, b"frame=0\nprogress=maybe\n");

    let reordered = wait_bounded(spawn_piped(&["progress-reordered"]));
    assert_eq!(
        reordered.stdout.bytes,
        b"progress=end\nframe=0\nout_time_us=0\n"
    );

    let truncated = wait_bounded(spawn_piped(&["progress-truncated"]));
    assert_eq!(truncated.stdout.bytes, b"frame=0\nout_time_us=");

    let started = Instant::now();
    let stalled = wait_bounded(spawn_piped(&["progress-stall", "--stall-ms", "120"]));
    assert!(stalled.status.success());
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert!(stalled.stdout.bytes.ends_with(b"progress=end\n"));
}

#[test]
fn spawned_control_delayed_and_ignored_behaviors_are_observable() {
    let mut child = Command::new(fake_child())
        .args([
            "control-delay",
            "--delay-ms",
            "50",
            "--max-control-bytes",
            "16",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("fake-child binary must spawn directly without a shell");
    child
        .stdin
        .take()
        .expect("piped stdin must exist")
        .write_all(b"stop\n")
        .expect("bounded command write must succeed");
    let delayed = wait_bounded(child);
    assert!(delayed.status.success());
    assert_eq!(delayed.stdout.bytes, b"control=acknowledged\n");
    assert!(
        String::from_utf8_lossy(&delayed.stderr.bytes)
            .starts_with("FFORAGER_FAKE_CHILD_CONTROL_READY")
    );

    let ignored = wait_bounded(spawn_piped(&["control-ignore", "--duration-ms", "100"]));
    assert!(ignored.status.success());
    let ignored_stderr = String::from_utf8_lossy(&ignored.stderr.bytes);
    assert!(ignored_stderr.contains("behavior=ignore"));
    assert!(ignored_stderr.contains("CONTROL_WINDOW_EXPIRED"));
}

#[test]
fn spawned_control_rejects_trailing_input() {
    let mut child = Command::new(fake_child())
        .args([
            "control-delay",
            "--delay-ms",
            "0",
            "--max-control-bytes",
            "16",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("negative fake-child binary must spawn");
    child
        .stdin
        .take()
        .expect("piped stdin must exist")
        .write_all(b"stop\njunk\n")
        .expect("bounded counterexample write must succeed");
    let output = wait_bounded(child);
    assert_eq!(output.status.code(), Some(65));
    assert!(output.stdout.bytes.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr.bytes).contains("must end after"));
}

#[test]
fn nonzero_partial_stale_and_read_only_outputs_have_distinct_oracles() {
    let crash = wait_bounded(spawn_piped(&[
        "crash",
        "--exit-code",
        "23",
        "--delay-ms",
        "0",
    ]));
    assert_eq!(crash.status.code(), Some(23));

    let directory = artifact_case("output-oracles");
    let partial = directory.join("partial.bin");
    let partial_result = wait_bounded(spawn_piped(&[
        "partial-output",
        "--output",
        partial.to_str().expect("artifact path must be Unicode"),
        "--bytes",
        "257",
        "--exit-code",
        "24",
    ]));
    assert_eq!(partial_result.status.code(), Some(24));
    assert_eq!(
        fs::metadata(&partial)
            .expect("partial file must exist")
            .len(),
        257
    );

    let stale = directory.join("stale.bin");
    fs::write(&stale, b"stale-sentinel").expect("stale fixture must be written");
    let stale_result = wait_bounded(spawn_piped(&[
        "stale-output",
        "--output",
        stale.to_str().expect("artifact path must be Unicode"),
    ]));
    assert!(stale_result.status.success());
    assert_eq!(
        fs::read(&stale).expect("stale fixture must remain"),
        b"stale-sentinel"
    );

    let read_only = directory.join("read-only.bin");
    fs::write(&read_only, b"read-only-sentinel").expect("read-only fixture must be written");
    let original_permissions = fs::metadata(&read_only)
        .expect("read-only fixture metadata must exist")
        .permissions();
    let mut read_only_permissions = original_permissions.clone();
    read_only_permissions.set_readonly(true);
    fs::set_permissions(&read_only, read_only_permissions)
        .expect("read-only fixture must be protected");
    let read_only_result = wait_bounded(spawn_piped(&[
        "read-only-output",
        "--output",
        read_only.to_str().expect("artifact path must be Unicode"),
    ]));
    assert!(read_only_result.status.success());
    assert_eq!(
        fs::read(&read_only).expect("read-only fixture must remain"),
        b"read-only-sentinel"
    );
    fs::set_permissions(&read_only, original_permissions)
        .expect("fixture cleanup must restore permissions");
    fs::remove_dir_all(directory).expect("artifact-contained output fixtures must clean up");
}

#[test]
fn bounded_descendant_outlives_direct_child_then_self_cleans() {
    let directory = artifact_case("descendant");
    let marker = directory.join("descendant-alive.marker");
    let mut child = spawn_piped(&[
        "descendant",
        "--lifetime-ms",
        "1000",
        "--liveness-file",
        marker.to_str().expect("artifact path must be Unicode"),
    ]);
    #[cfg(windows)]
    let direct_status = {
        let started = Instant::now();
        loop {
            if let Some(status) = child
                .try_wait()
                .expect("direct-child status must be observable")
            {
                break status;
            }
            if started.elapsed() >= Duration::from_secs(2) {
                let diagnostics = descendant_timeout_diagnostics(&mut child, &marker);
                panic!("direct child did not exit within 2s; {diagnostics}");
            }
            thread::sleep(Duration::from_millis(10));
        }
    };
    #[cfg(not(windows))]
    let direct_status = wait_for_direct_exit(&mut child, Duration::from_secs(2));
    assert!(direct_status.success());
    assert!(
        marker.is_file(),
        "descendant must outlive the reaped direct child"
    );
    let output = wait_bounded(child);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout.bytes).contains("DESCENDANT pid="));
    wait_until_absent(&marker, Duration::from_secs(1));
    fs::remove_dir_all(directory).expect("artifact-contained descendant fixture must clean up");
}

#[cfg(unix)]
#[test]
fn setsid_escape_fixture_outlives_direct_child_and_retains_escape_label() {
    let setsid = Path::new("/usr/bin/setsid");
    assert!(
        setsid.is_file(),
        "Linux-native proof host must provide /usr/bin/setsid"
    );
    let directory = artifact_case("setsid-escape");
    let marker = directory.join("setsid-alive.marker");
    let mut child = spawn_piped(&[
        "setsid-escape",
        "--setsid-path",
        setsid.to_str().expect("setsid path must be Unicode"),
        "--lifetime-ms",
        "1000",
        "--liveness-file",
        marker.to_str().expect("artifact path must be Unicode"),
    ]);
    let direct_status = wait_for_direct_exit(&mut child, Duration::from_secs(2));
    assert!(direct_status.success());
    assert!(
        marker.is_file(),
        "setsid descendant must outlive the direct child"
    );
    let output = wait_bounded(child);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout.bytes).contains("SETSID_ESCAPE pid="));
    wait_until_absent(&marker, Duration::from_secs(1));
    fs::remove_dir_all(directory).expect("artifact-contained setsid fixture must clean up");
}

#[test]
fn exit_kill_and_reap_race_exposes_natural_and_forced_outcomes() {
    let natural_zero = wait_bounded(spawn_piped(&[
        "race-exit",
        "--exit-delay-ms",
        "0",
        "--exit-code",
        "0",
    ]));
    assert!(natural_zero.status.success());
    assert_eq!(
        natural_zero.stdout.bytes,
        b"FFORAGER_FAKE_CHILD_RACE_READY\n"
    );

    let natural_nonzero = wait_bounded(spawn_piped(&[
        "race-exit",
        "--exit-delay-ms",
        "10",
        "--exit-code",
        "29",
    ]));
    assert_eq!(natural_nonzero.status.code(), Some(29));

    let mut forced = spawn_piped(&["race-exit", "--exit-delay-ms", "1000", "--exit-code", "0"]);
    thread::sleep(Duration::from_millis(50));
    forced
        .kill()
        .expect("test may terminate only the long-lived race child it started");
    let forced = wait_bounded(forced);
    assert!(!forced.status.success());
}

#[cfg(windows)]
#[test]
fn windows_inherited_stdout_handle_sentinel_outlives_the_direct_child() {
    let directory = artifact_case("windows-handle-inheritance");
    let marker = directory.join("handle-holder-alive.marker");
    let mut child = spawn_piped(&[
        "windows-handle-inheritance",
        "--lifetime-ms",
        "1000",
        "--liveness-file",
        marker.to_str().expect("artifact path must be Unicode"),
    ]);
    let direct_status = wait_for_direct_exit(&mut child, Duration::from_secs(2));
    assert!(direct_status.success());
    assert!(
        marker.is_file(),
        "the deliberately inherited stdout handle holder must outlive the direct child"
    );
    let started = Instant::now();
    let output = wait_bounded(child);
    assert!(output.status.success());
    assert!(
        started.elapsed() >= Duration::from_millis(500),
        "stdout EOF must be delayed while the descendant retains the inherited handle"
    );
    assert!(String::from_utf8_lossy(&output.stdout.bytes).contains("WINDOWS_HANDLE_LEAK"));
    wait_until_absent(&marker, Duration::from_secs(1));
    fs::remove_dir_all(directory).expect("artifact-contained handle fixture must clean up");
}
