use crate::{
    one_shot_api::{ExactLaunch, OneShotEvent, OneShotReason, OneShotRequest, OneShotStartError,
        OneShotTerminal, OutputBounds, OutputStream, OneShotObservedFailure, OneShotObservedStartError,
        OneShotProcess, OneShotLaunchError},
    raw_pipe::{Pipes, Reader},
    supervisor::{ProcessOwner, quiet},
    Permit, Supervisor, WAIT_SLICE,
};
use hsk_studio_accord::CancellationToken;
use std::{ffi::OsStr, io, process::{Command, Stdio}, thread, time::{Duration, Instant}};

/// One admitted process. Output is pulled by this iterator, with no reader threads or queues.
/// Dropping it reclaims its child; failed reclamation remains owned by the supervisor.
pub struct OneShotSession {
    stdout: Option<Reader>,
    stderr: Option<Reader>,
    provided_output: Option<[bool; 2]>,
    process: Option<ProcessOwner>,
    cancel: CancellationToken,
    deadline: Instant,
    bounds: OutputBounds,
    stdout_bytes: u64,
    stderr_bytes: u64,
    stderr_first: bool,
    pid: u32,
}

fn invalid_os(value: &OsStr, key: bool) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        value.as_bytes().iter().any(|byte| *byte == 0 || (key && *byte == b'='))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        value.encode_wide().any(|unit| unit == 0 || (key && unit == u16::from(b'=')))
    }
    #[cfg(not(any(unix, windows)))]
    { let _ = (value, key); true }
}

fn valid_launch(launch: &ExactLaunch<'_>) -> bool {
    if !launch.executable.is_absolute() || !launch.cwd.is_absolute()
        || invalid_os(launch.executable.as_os_str(), false) || invalid_os(launch.cwd.as_os_str(), false)
        || launch.args.iter().any(|arg| invalid_os(arg, false))
        || launch.env.iter().any(|(key, value)| key.is_empty() || invalid_os(key, true) || invalid_os(value, false)) {
        return false;
    }
    #[cfg(windows)]
    {
        if launch.executable.extension().and_then(OsStr::to_str)
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")) {
            return false;
        }
        let keys: Vec<_> = launch.env.keys().collect();
        for (index, left) in keys.iter().enumerate() {
            for right in keys.iter().skip(index.saturating_add(1)) {
                if !matches!(crate::raw_pipe::same_environment_key(left, right), Ok(false)) {
                    return false;
                }
            }
        }
    }
    true
}

impl Supervisor {
    /// Runs an exact creation-time spawner after worker admission/preflight. The spawner must enforce
    /// its own approved creation-time policy and empty inherited environment. It must return genuine
    /// custody for EVERY post-creation failure; a panic before transfer must reclaim its child.
    /// Configured supervisor OS caps are refused because this path cannot apply them pre-creation.
    pub fn start_one_shot_with_spawner<'a, S, O>(
        &self, permit: Permit, request: OneShotRequest<'a>, cancel: CancellationToken,
        deadline: Instant, spawn: S, observer: O,
    ) -> Result<OneShotSession, OneShotObservedStartError>
    where
        S: FnOnce(ExactLaunch<'a>, &CancellationToken, Instant)
            -> Result<Box<dyn OneShotProcess>, OneShotLaunchError>,
        O: FnOnce(u32) -> Result<(), ()>,
    {
        use OneShotObservedStartError::BeforeSpawn;
        if cancel.check().is_err() { return Err(BeforeSpawn(OneShotStartError::Canceled)); }
        if Instant::now() >= deadline { return Err(BeforeSpawn(OneShotStartError::Deadline)); }
        if self.requests_os_caps() { return Err(BeforeSpawn(OneShotStartError::UnsupportedCaps)); }
        if !self.owns_permit(&permit) || request.bounds.chunk_bytes.get() > 65_536
            || !valid_launch(&request.launch) {
            return Err(BeforeSpawn(OneShotStartError::InvalidLaunch));
        }
        let (child, startup_failed) = match spawn(request.launch, &cancel, deadline) {
            Ok(child) => (child, false),
            Err(OneShotLaunchError::BeforeSpawn) => return Err(BeforeSpawn(
                if cancel.check().is_err() { OneShotStartError::Canceled }
                else if Instant::now() >= deadline { OneShotStartError::Deadline }
                else { OneShotStartError::NotStarted })),
            Err(OneShotLaunchError::AfterSpawn(child)) => (child, true),
        };
        // Even a late or failed launch is adopted before observation and cleanup.
        let process = self.adopt_provided(child, permit, request.owner_session, request.keepalive);
        let process = observe_owned(process, observer)?;
        if startup_failed {
            return Err(observed_failure(process, OneShotObservedFailure::Setup));
        }
        if cancel.check().is_err() {
            process.canceled();
            return Err(observed_failure(process, OneShotObservedFailure::Setup));
        }
        if Instant::now() >= deadline {
            process.timed_out();
            return Err(observed_failure(process, OneShotObservedFailure::Setup));
        }
        let pid = process.pid();
        Ok(OneShotSession {
            stdout: None, stderr: None, provided_output: Some([true, true]), process: Some(process),
            cancel, deadline, bounds: request.bounds, stdout_bytes: 0, stderr_bytes: 0,
            stderr_first: false, pid,
        })
    }

    /// Starts an exact native command under this supervisor's admission and lifecycle ownership.
    /// Capped one-shot execution is rejected before spawn because post-spawn attachment is too late.
    pub fn start_one_shot(
        &self,
        permit: Permit,
        request: OneShotRequest<'_>,
        cancel: CancellationToken,
        deadline: Instant,
    ) -> Result<OneShotSession, OneShotStartError> {
        self.start_one_shot_observed(permit, request, cancel, deadline, |_| Ok(()))
            .map_err(|error| match error {
                OneShotObservedStartError::BeforeSpawn(error) => error,
                OneShotObservedStartError::AfterSpawn { cleanup, .. } => OneShotStartError::Setup(cleanup),
            })
    }

    /// Observes the real PID once, immediately after child adoption and before output setup.
    /// The synchronous observer must not block. Rejection attempts reclamation. An unwind panic
    /// resumes with its original payload after cleanup or supervisor retention of the child.
    /// Normal Rust panic hooks may run; aborts are not caught.
    pub fn start_one_shot_observed<F>(
        &self,
        permit: Permit,
        request: OneShotRequest<'_>,
        cancel: CancellationToken,
        deadline: Instant,
        observer: F,
    ) -> Result<OneShotSession, OneShotObservedStartError>
    where F: FnOnce(u32) -> Result<(), ()> {
        self.start_one_shot_with_setup(permit, request, cancel, deadline, observer,
            |pipes, process| match process.child_mut() {
                Some(child) => pipes.readers(child),
                None => Err(io::ErrorKind::BrokenPipe.into()),
            })
    }

    fn start_one_shot_with_setup<F, S>(
        &self, permit: Permit, request: OneShotRequest<'_>, cancel: CancellationToken,
        deadline: Instant, observer: F, setup: S,
    ) -> Result<OneShotSession, OneShotObservedStartError>
    where
        F: FnOnce(u32) -> Result<(), ()>,
        S: FnOnce(Pipes, &mut ProcessOwner) -> io::Result<(Reader, Reader)>,
    {
        use OneShotObservedStartError::BeforeSpawn;
        if cancel.check().is_err() { return Err(BeforeSpawn(OneShotStartError::Canceled)); }
        if Instant::now() >= deadline { return Err(BeforeSpawn(OneShotStartError::Deadline)); }
        if !cfg!(any(unix, windows)) { return Err(BeforeSpawn(OneShotStartError::UnsupportedPlatform)); }
        if self.requests_os_caps() { return Err(BeforeSpawn(OneShotStartError::UnsupportedCaps)); }
        if !self.owns_permit(&permit) || request.bounds.chunk_bytes.get() > 65_536
            || !valid_launch(&request.launch) {
            return Err(BeforeSpawn(OneShotStartError::InvalidLaunch));
        }
        let mut command = Command::new(request.launch.executable);
        command.args(request.launch.args).current_dir(request.launch.cwd)
            .env_clear().envs(request.launch.env).stdin(Stdio::null());
        quiet(&mut command);
        let pipes = Pipes::prepare(&mut command).map_err(|_| BeforeSpawn(OneShotStartError::NotStarted))?;
        // Recheck after command/pipe setup, immediately before the only spawn boundary.
        if cancel.check().is_err() { return Err(BeforeSpawn(OneShotStartError::Canceled)); }
        if Instant::now() >= deadline { return Err(BeforeSpawn(OneShotStartError::Deadline)); }
        let child = command.spawn().map_err(|_| BeforeSpawn(OneShotStartError::NotStarted))?;
        let process = self.adopt(child, permit, request.owner_session, request.keepalive);
        let pid = process.pid();
        let mut process = observe_owned(process, observer)?;
        let (stdout, stderr) = match setup(pipes, &mut process) {
            Ok(readers) => readers,
            Err(_) => return Err(observed_failure(process, OneShotObservedFailure::Setup)),
        };
        Ok(OneShotSession {
            stdout: Some(stdout), stderr: Some(stderr), provided_output: None, process: Some(process), cancel, deadline,
            bounds: request.bounds, stdout_bytes: 0, stderr_bytes: 0, stderr_first: false, pid,
        })
    }
}

fn observe_owned<O>(mut process: ProcessOwner, observer: O)
    -> Result<ProcessOwner, OneShotObservedStartError>
where O: FnOnce(u32) -> Result<(), ()> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer(process.pid()))) {
        Ok(Ok(())) => Ok(process),
        Ok(Err(())) => Err(observed_failure(process, OneShotObservedFailure::ObserverRejected)),
        Err(payload) => {
            // Do not invoke arbitrary provider cleanup while preserving the original opaque panic.
            if !process.defer_provided_cleanup() {
                let _ = process.terminate(Duration::ZERO);
            }
            drop(process);
            std::panic::resume_unwind(payload)
        }
    }
}

fn observed_failure(mut process: ProcessOwner, cause: OneShotObservedFailure) -> OneShotObservedStartError {
    let pid = process.pid();
    let cleanup = process.terminate(Duration::ZERO);
    // Requeue failed reaps before exposing the error. Keepalive and admission remain owned there.
    drop(process);
    OneShotObservedStartError::AfterSpawn { pid, cause, cleanup }
}

impl OneShotSession {
    pub fn pid(&self) -> u32 { self.pid }

    fn control(&self) -> Option<OneShotReason> {
        if self.cancel.check().is_err() { Some(OneShotReason::Canceled) }
        else if Instant::now() >= self.deadline { Some(OneShotReason::Deadline) }
        else { None }
    }

    fn finish(&mut self, reason: OneShotReason) -> Option<OneShotEvent> {
        let reason = self.control().unwrap_or(reason);
        let mut process = self.process.take()?;
        self.stdout.take();
        self.stderr.take();
        match reason {
            OneShotReason::Canceled => process.canceled(),
            OneShotReason::Deadline => process.timed_out(),
            OneShotReason::Exited { success: false, .. } | OneShotReason::Io => process.crashed(),
            _ => {}
        }
        let cleanup = process.terminate(Duration::ZERO);
        let reason = if cleanup.reaped { reason } else { OneShotReason::ReclamationPending };
        // The owner moves failed reaps into the supervisor's pending cleanup before returning.
        drop(process);
        Some(OneShotEvent::Terminal(OneShotTerminal { reason, cleanup }))
    }

    fn read_stream(&mut self, stream: OutputStream) -> io::Result<Option<Vec<u8>>> {
        let (reader, used, limit) = match stream {
            OutputStream::Stdout => (&mut self.stdout, &mut self.stdout_bytes, self.bounds.stdout_bytes),
            OutputStream::Stderr => (&mut self.stderr, &mut self.stderr_bytes, self.bounds.stderr_bytes),
        };
        let provided_open = self.provided_output.as_mut().map(|open| match stream {
            OutputStream::Stdout => &mut open[0], OutputStream::Stderr => &mut open[1],
        });
        if provided_open.as_ref().is_some_and(|open| !**open)
            || (provided_open.is_none() && reader.is_none()) { return Ok(None); }
        let available = limit.saturating_sub(*used);
        let length = self.bounds.chunk_bytes.get().min(
            usize::try_from(available.saturating_add(1)).unwrap_or(usize::MAX));
        let mut bytes = vec![0; length];
        let result = if provided_open.is_some() {
            self.process.as_mut().ok_or(io::ErrorKind::BrokenPipe)?.try_read_provided(stream, &mut bytes)
        } else {
            reader.as_mut().ok_or(io::ErrorKind::BrokenPipe)?.read(&mut bytes)
        };
        match result {
            Ok(0) => {
                if let Some(open) = provided_open { *open = false; } else { *reader = None; }
                Ok(None)
            }
            Ok(count) => {
                if count > bytes.len() { return Err(io::ErrorKind::InvalidData.into()); }
                bytes.truncate(count);
                let count = u64::try_from(count).map_err(|_| io::ErrorKind::InvalidData)?;
                if count > available { return Err(io::ErrorKind::FileTooLarge.into()); }
                *used = used.saturating_add(count);
                Ok(Some(bytes))
            }
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl Iterator for OneShotSession {
    type Item = OneShotEvent;

    fn next(&mut self) -> Option<Self::Item> {
        self.process.as_ref()?;
        loop {
            if let Some(reason) = self.control() { return self.finish(reason); }
            let streams = if self.stderr_first { [OutputStream::Stderr, OutputStream::Stdout] }
                else { [OutputStream::Stdout, OutputStream::Stderr] };
            for stream in streams {
                if let Some(reason) = self.control() { return self.finish(reason); }
                match self.read_stream(stream) {
                    Ok(Some(bytes)) => {
                        if let Some(reason) = self.control() { return self.finish(reason); }
                        self.stderr_first = matches!(stream, OutputStream::Stdout);
                        return Some(OneShotEvent::Output { stream, bytes });
                    }
                    Ok(None) => {}
                    Err(error) => return self.finish(if error.kind() == io::ErrorKind::FileTooLarge {
                        OneShotReason::OutputLimit(stream)
                    } else { OneShotReason::Io }),
                }
            }
            let streams_closed = self.provided_output.map_or(
                self.stdout.is_none() && self.stderr.is_none(), |open| !open[0] && !open[1]);
            if streams_closed {
                let status = self.process.as_mut().map(ProcessOwner::try_wait);
                match status {
                    Some(Ok(Some(status))) => {
                        if let Some(reason) = self.control() { return self.finish(reason); }
                        return self.finish(OneShotReason::Exited { success: status.success(), code: status.code() });
                    }
                    Some(Ok(None)) => {}
                    _ => return self.finish(OneShotReason::Io),
                }
            }
            thread::sleep(WAIT_SLICE.min(self.deadline.saturating_duration_since(Instant::now())));
        }
    }
}

impl std::iter::FusedIterator for OneShotSession {}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;
    use std::{cell::Cell, collections::BTreeMap, num::NonZeroUsize, sync::{Arc, atomic::{AtomicBool, Ordering}}};

    struct KeepaliveProbe(Arc<AtomicBool>);
    impl Drop for KeepaliveProbe {
        fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); }
    }

    struct ProvidedTestProcess {
        child: std::process::Child,
        stdout: Option<Reader>,
        stderr: Option<Reader>,
        status: Option<std::process::ExitStatus>,
        killed: bool,
        cleanup_allowed: Arc<AtomicBool>,
        panic_on_terminate: Arc<AtomicBool>,
    }

    impl OneShotProcess for ProvidedTestProcess {
        fn pid(&self) -> u32 { self.child.id() }
        fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
            if self.status.is_none() { self.status = self.child.try_wait()?; }
            Ok(self.status)
        }
        fn terminate(&mut self, _patience: Duration) -> crate::Terminated {
            assert!(!self.panic_on_terminate.load(Ordering::SeqCst), "test provider cleanup panic");
            if self.try_wait().ok().flatten().is_none() {
                self.killed |= self.child.kill().is_ok();
                let until = Instant::now() + crate::EXIT_PATIENCE;
                while self.try_wait().ok().flatten().is_none() && Instant::now() < until {
                    thread::sleep(WAIT_SLICE);
                }
            }
            crate::Terminated {
                killed: self.killed,
                reaped: self.status.is_some() && self.cleanup_allowed.load(Ordering::SeqCst),
                exit_code: self.status.and_then(|status| status.code()),
            }
        }
        fn try_read(&mut self, stream: OutputStream, buffer: &mut [u8]) -> io::Result<usize> {
            let reader = match stream { OutputStream::Stdout => &mut self.stdout, OutputStream::Stderr => &mut self.stderr };
            reader.as_mut().ok_or(io::ErrorKind::BrokenPipe)?.read(buffer)
        }
    }

    impl Drop for ProvidedTestProcess {
        fn drop(&mut self) {
            self.panic_on_terminate.store(false, Ordering::SeqCst);
            self.cleanup_allowed.store(true, Ordering::SeqCst);
            let _ = self.terminate(Duration::ZERO);
        }
    }

    fn spawn_provided_test(launch: ExactLaunch<'_>, cleanup_allowed: Arc<AtomicBool>,
        panic_on_terminate: Arc<AtomicBool>) -> Result<Box<dyn OneShotProcess>, OneShotLaunchError> {
        let mut command = Command::new(launch.executable);
        command.args(launch.args).current_dir(launch.cwd).env_clear().envs(launch.env).stdin(Stdio::null());
        quiet(&mut command);
        let pipes = Pipes::prepare(&mut command).map_err(|_| OneShotLaunchError::BeforeSpawn)?;
        let child = command.spawn().map_err(|_| OneShotLaunchError::BeforeSpawn)?;
        let mut process = ProvidedTestProcess { child, stdout: None, stderr: None, status: None,
            killed: false, cleanup_allowed, panic_on_terminate };
        match pipes.readers(&mut process.child) {
            Ok((stdout, stderr)) => { process.stdout = Some(stdout); process.stderr = Some(stderr); Ok(Box::new(process)) }
            Err(_) => Err(OneShotLaunchError::AfterSpawn(Box::new(process))),
        }
    }

    fn provided_supervisor(program: &std::path::Path) -> Supervisor {
        Supervisor::new(crate::SupervisorConfig {
            program: program.to_owned(), args: vec![], env: vec![], max_workers: 1,
            max_in_flight_bytes: 4096, spawn_timeout: Duration::from_secs(10),
            os_limits: None, reply_buffer: None, io_thread_name: "unused".into(),
        })
    }

    fn provided_request<'a>(program: &'a std::path::Path, args: &'a [std::ffi::OsString],
        env: &'a BTreeMap<std::ffi::OsString, std::ffi::OsString>, keepalive: Option<Box<dyn Send>>)
        -> OneShotRequest<'a> {
        OneShotRequest {
            owner_session: "provided-test",
            launch: ExactLaunch { executable: program, cwd: program.parent().unwrap(), args, env },
            bounds: OutputBounds { chunk_bytes: NonZeroUsize::new(128).unwrap(), stdout_bytes: 65_536, stderr_bytes: 4096 },
            keepalive,
        }
    }

    #[test]
    fn provided_process_retains_admission_and_keepalive_until_full_cleanup() {
        let program = std::env::current_exe().unwrap();
        let sup = provided_supervisor(&program);
        let args = ["--list".into()];
        let env = BTreeMap::new();
        let allow = Arc::new(AtomicBool::new(false));
        let released = Arc::new(AtomicBool::new(false));
        let seen = Cell::new(0);
        let mut session = sup.start_one_shot_with_spawner(sup.admit(32).unwrap(),
            provided_request(&program, &args, &env, Some(Box::new(KeepaliveProbe(released.clone())))),
            CancellationToken::default(), Instant::now() + Duration::from_secs(10),
            |launch, _, _| spawn_provided_test(launch, allow.clone(), Arc::new(AtomicBool::new(false))),
            |pid| { seen.set(pid); Ok(()) }).unwrap();
        assert_eq!(session.pid(), seen.get());
        let mut output = Vec::new();
        let mut terminal = None;
        for event in &mut session {
            match event {
                OneShotEvent::Output { stream: OutputStream::Stdout, bytes } => output.extend(bytes),
                OneShotEvent::Terminal(done) => terminal = Some(done),
                _ => {},
            }
        }
        assert!(!output.is_empty());
        let terminal = terminal.unwrap();
        assert_eq!(terminal.reason, OneShotReason::ReclamationPending);
        assert!(!terminal.cleanup.reaped);
        assert!(!sup.ledger()[0].reaped);
        assert!(sup.admit(0).is_err());
        assert!(!released.load(Ordering::SeqCst));
        assert_eq!(sup.retry_cleanup(), 1);
        allow.store(true, Ordering::SeqCst);
        assert_eq!(sup.retry_cleanup(), 0);
        assert!(sup.ledger()[0].reaped);
        assert_eq!(sup.stats().workers_live, 0);
        assert!(released.load(Ordering::SeqCst));
    }

    #[test]
    fn provided_startup_failure_is_observed_and_cleaned_with_real_identity() {
        let program = std::env::current_exe().unwrap();
        let sup = provided_supervisor(&program);
        let args = ["--list".into()];
        let env = BTreeMap::new();
        let seen = Cell::new(0);
        let error = sup.start_one_shot_with_spawner(sup.admit(0).unwrap(),
            provided_request(&program, &args, &env, None), CancellationToken::default(),
            Instant::now() + Duration::from_secs(10), |launch, _, _| {
                let child = spawn_provided_test(launch, Arc::new(AtomicBool::new(true)), Arc::new(AtomicBool::new(false)))?;
                Err(OneShotLaunchError::AfterSpawn(child))
            }, |pid| { seen.set(pid); Ok(()) }).err().unwrap();
        let OneShotObservedStartError::AfterSpawn { pid, cause, cleanup } = error else { panic!("missing real identity"); };
        assert_eq!(pid, seen.get());
        assert_eq!(cause, OneShotObservedFailure::Setup);
        assert!(cleanup.reaped);
        assert_eq!(sup.ledger()[0].pid, pid);
        assert_eq!(sup.stats().workers_live, 0);
    }

    #[test]
    fn provided_late_canceled_launch_is_adopted_before_cleanup() {
        let program = std::env::current_exe().unwrap();
        let sup = provided_supervisor(&program);
        let args = ["--list".into()];
        let env = BTreeMap::new();
        let seen = Cell::new(0);
        let deadline = Instant::now() + Duration::from_secs(10);
        let error = sup.start_one_shot_with_spawner(sup.admit(0).unwrap(),
            provided_request(&program, &args, &env, None), CancellationToken::default(), deadline,
            |launch, cancel, passed_deadline| {
                assert_eq!(passed_deadline, deadline);
                let child = spawn_provided_test(launch, Arc::new(AtomicBool::new(true)), Arc::new(AtomicBool::new(false)))?;
                cancel.cancel();
                Ok(child)
            }, |pid| { seen.set(pid); Ok(()) }).err().unwrap();
        let OneShotObservedStartError::AfterSpawn { pid, cleanup, .. } = error else { panic!("missing identity"); };
        assert_eq!(pid, seen.get());
        assert!(cleanup.reaped);
        assert_eq!(sup.stats().canceled, 1);
    }

    #[cfg(panic = "unwind")]
    #[test]
    fn provided_cleanup_panic_retains_exact_process_before_propagation() {
        let program = std::env::current_exe().unwrap();
        let sup = provided_supervisor(&program);
        let args = ["--list".into()];
        let env = BTreeMap::new();
        let panic_cleanup = Arc::new(AtomicBool::new(true));
        let seen = Cell::new(0);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sup.start_one_shot_with_spawner(sup.admit(0).unwrap(),
                provided_request(&program, &args, &env, None), CancellationToken::default(),
                Instant::now() + Duration::from_secs(10), |launch, _, _|
                    spawn_provided_test(launch, Arc::new(AtomicBool::new(true)), panic_cleanup.clone()),
                |pid| { seen.set(pid); Err(()) })
        }));
        assert!(result.is_err());
        assert_eq!(sup.ledger()[0].pid, seen.get());
        assert!(!sup.ledger()[0].reaped);
        assert!(sup.admit(0).is_err());
        panic_cleanup.store(false, Ordering::SeqCst);
        assert_eq!(sup.retry_cleanup(), 0);
        assert!(sup.ledger()[0].reaped);
        assert_eq!(sup.stats().workers_live, 0);
    }

    #[cfg(panic = "unwind")]
    #[test]
    fn provided_retry_panic_preserves_unprocessed_owner_admission_and_keepalive() {
        let program = std::env::current_exe().unwrap();
        let sup = Supervisor::new(crate::SupervisorConfig {
            program: program.clone(), args: vec![], env: vec![], max_workers: 2,
            max_in_flight_bytes: 4096, spawn_timeout: Duration::from_secs(10),
            os_limits: None, reply_buffer: None, io_thread_name: "unused".into(),
        });
        let args = ["--list".into()];
        let env = BTreeMap::new();
        let allow_first = Arc::new(AtomicBool::new(false));
        let allow_second = Arc::new(AtomicBool::new(false));
        let panic_first = Arc::new(AtomicBool::new(false));
        let released_first = Arc::new(AtomicBool::new(false));
        let released_second = Arc::new(AtomicBool::new(false));
        let mut pids = Vec::new();
        for (allow, panic_cleanup, released) in [
            (allow_first.clone(), panic_first.clone(), released_first.clone()),
            (allow_second.clone(), Arc::new(AtomicBool::new(false)), released_second.clone()),
        ] {
            let session = sup.start_one_shot_with_spawner(sup.admit(32).unwrap(),
                provided_request(&program, &args, &env, Some(Box::new(KeepaliveProbe(released)))),
                CancellationToken::default(), Instant::now() + Duration::from_secs(10),
                |launch, _, _| spawn_provided_test(launch, allow, panic_cleanup), |_| Ok(())).unwrap();
            pids.push(session.pid());
            drop(session);
        }
        assert_ne!(pids[0], pids[1]);
        assert_eq!(sup.stats().workers_live, 2);
        assert_eq!(sup.stats().in_flight_bytes, 64);
        panic_first.store(true, Ordering::SeqCst);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sup.retry_cleanup()));
        assert!(result.is_err());
        assert!(!released_first.load(Ordering::SeqCst));
        assert!(!released_second.load(Ordering::SeqCst));
        assert_eq!(sup.stats().workers_live, 2);
        assert_eq!(sup.stats().in_flight_bytes, 64);
        assert!(sup.admit(0).is_err());
        let ledger = sup.ledger();
        assert_eq!(ledger.len(), 2);
        assert_eq!([ledger[0].pid, ledger[1].pid], [pids[0], pids[1]]);
        assert!(ledger.iter().all(|entry| !entry.reaped));
        panic_first.store(false, Ordering::SeqCst);
        allow_first.store(true, Ordering::SeqCst);
        allow_second.store(true, Ordering::SeqCst);
        assert_eq!(sup.retry_cleanup(), 0);
        assert!(sup.ledger().iter().all(|entry| entry.reaped));
        assert!(released_first.load(Ordering::SeqCst));
        assert!(released_second.load(Ordering::SeqCst));
        assert_eq!(sup.stats().workers_live, 0);
        assert_eq!(sup.stats().in_flight_bytes, 0);
    }

    struct CountedTermination {
        process: Box<dyn OneShotProcess>,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl OneShotProcess for CountedTermination {
        fn pid(&self) -> u32 { self.process.pid() }
        fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> { self.process.try_wait() }
        fn terminate(&mut self, patience: Duration) -> crate::Terminated {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.process.terminate(patience)
        }
        fn try_read(&mut self, stream: OutputStream, buffer: &mut [u8]) -> io::Result<usize> {
            self.process.try_read(stream, buffer)
        }
    }

    #[cfg(panic = "unwind")]
    #[test]
    fn provided_observer_panic_preserves_original_payload_without_running_provider_cleanup() {
        struct OriginalPayload { identity: Arc<()>, dropped: Arc<AtomicBool> }
        impl Drop for OriginalPayload {
            fn drop(&mut self) { self.dropped.store(true, Ordering::SeqCst); }
        }
        let program = std::env::current_exe().unwrap();
        let sup = provided_supervisor(&program);
        let args = ["--list".into()];
        let env = BTreeMap::new();
        let identity = Arc::new(());
        let payload_dropped = Arc::new(AtomicBool::new(false));
        let original = OriginalPayload { identity: identity.clone(), dropped: payload_dropped.clone() };
        let released = Arc::new(AtomicBool::new(false));
        let cleanup_panics = Arc::new(AtomicBool::new(true));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let pid = Cell::new(0);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sup.start_one_shot_with_spawner(sup.admit(32).unwrap(),
                provided_request(&program, &args, &env, Some(Box::new(KeepaliveProbe(released.clone())))),
                CancellationToken::default(), Instant::now() + Duration::from_secs(10),
                |launch, _, _| {
                    let process = spawn_provided_test(launch, Arc::new(AtomicBool::new(true)), cleanup_panics.clone())?;
                    Ok(Box::new(CountedTermination { process, calls: calls.clone() }))
                }, |observed| { pid.set(observed); std::panic::panic_any(original) })
        }));
        let payload = result.err().expect("original observer panic");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!payload_dropped.load(Ordering::SeqCst));
        let payload = payload.downcast::<OriginalPayload>().unwrap_or_else(|_| panic!("replaced original payload"));
        assert!(Arc::ptr_eq(&payload.identity, &identity));
        drop(payload);
        assert!(payload_dropped.load(Ordering::SeqCst));
        assert_ne!(pid.get(), 0);
        assert_eq!(sup.ledger()[0].pid, pid.get());
        assert!(!sup.ledger()[0].reaped);
        assert!(!released.load(Ordering::SeqCst));
        assert_eq!(sup.stats().in_flight_bytes, 32);
        assert!(sup.admit(0).is_err());
        let retry = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sup.retry_cleanup()));
        assert!(retry.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(!released.load(Ordering::SeqCst));
        assert!(!sup.ledger()[0].reaped);
        cleanup_panics.store(false, Ordering::SeqCst);
        assert_eq!(sup.retry_cleanup(), 0);
        assert!(sup.ledger()[0].reaped);
        assert!(released.load(Ordering::SeqCst));
        assert_eq!(sup.stats().workers_live, 0);
    }

    struct OversizedRead(Box<dyn OneShotProcess>);
    impl OneShotProcess for OversizedRead {
        fn pid(&self) -> u32 { self.0.pid() }
        fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> { self.0.try_wait() }
        fn terminate(&mut self, patience: Duration) -> crate::Terminated { self.0.terminate(patience) }
        fn try_read(&mut self, _: OutputStream, buffer: &mut [u8]) -> io::Result<usize> {
            Ok(buffer.len().saturating_add(1))
        }
    }

    #[test]
    fn provided_oversized_read_count_fails_closed_and_reaps() {
        let program = std::env::current_exe().unwrap();
        let sup = provided_supervisor(&program);
        let args = ["--list".into()];
        let env = BTreeMap::new();
        let mut session = sup.start_one_shot_with_spawner(sup.admit(0).unwrap(),
            provided_request(&program, &args, &env, None), CancellationToken::default(),
            Instant::now() + Duration::from_secs(10), |launch, _, _| {
                let process = spawn_provided_test(launch, Arc::new(AtomicBool::new(true)), Arc::new(AtomicBool::new(false)))?;
                Ok(Box::new(OversizedRead(process)))
            }, |_| Ok(())).unwrap();
        let Some(OneShotEvent::Terminal(done)) = session.next() else { panic!("invalid read produced output"); };
        assert_eq!(done.reason, OneShotReason::Io);
        assert!(done.cleanup.reaped);
        assert!(sup.ledger()[0].reaped);
    }

    #[test]
    fn provided_preflight_refusal_does_not_call_spawner_or_observer() {
        let program = std::env::current_exe().unwrap();
        let sup = provided_supervisor(&program);
        let args = ["--list".into()];
        let env = BTreeMap::new();
        let cancel = CancellationToken::default();
        cancel.cancel();
        let error = sup.start_one_shot_with_spawner(sup.admit(0).unwrap(),
            provided_request(&program, &args, &env, None), cancel,
            Instant::now() + Duration::from_secs(10), |_, _, _| panic!("spawner must not run"),
            |_| panic!("observer must not run")).err().unwrap();
        assert_eq!(error, OneShotObservedStartError::BeforeSpawn(OneShotStartError::Canceled));
        assert!(sup.ledger().is_empty());
        assert_eq!(sup.stats().workers_live, 0);
    }

    #[test]
    fn observed_setup_failure_keeps_identity_and_owns_resources_through_cleanup() {
        // Spawn this test harness only to list tests. The injected setup failure is private and
        // cannot be selected through the product API or environment.
        let program = std::env::current_exe().expect("test executable");
        let sup = Supervisor::new(crate::SupervisorConfig {
            program: program.clone(), args: vec![], env: vec![], max_workers: 1,
            max_in_flight_bytes: 4096, spawn_timeout: Duration::from_secs(10),
            os_limits: None, reply_buffer: None, io_thread_name: "unused".into(),
        });
        let args = [std::ffi::OsString::from("--list")];
        let env = BTreeMap::new();
        let released = Arc::new(AtomicBool::new(false));
        let observed = Cell::new(0);
        let setups = Cell::new(0);
        let error = sup.start_one_shot_with_setup(sup.admit(32).unwrap(), OneShotRequest {
            owner_session: "setup-failure-test",
            launch: ExactLaunch { executable: &program, cwd: program.parent().unwrap(), args: &args, env: &env },
            bounds: OutputBounds { chunk_bytes: NonZeroUsize::new(128).unwrap(), stdout_bytes: 4096, stderr_bytes: 4096 },
            keepalive: Some(Box::new(KeepaliveProbe(released.clone()))),
        }, CancellationToken::default(), Instant::now() + Duration::from_secs(10), |pid| {
            observed.set(pid);
            assert!(!released.load(Ordering::SeqCst));
            Ok(())
        }, |_, _| {
            setups.set(setups.get() + 1);
            assert_ne!(observed.get(), 0, "observer precedes setup");
            assert!(!released.load(Ordering::SeqCst));
            assert!(sup.admit(0).is_err());
            Err(io::ErrorKind::BrokenPipe.into())
        }).err().expect("setup failure");
        let OneShotObservedStartError::AfterSpawn { pid, cause, cleanup } = error else { panic!("no spawned identity"); };
        assert_eq!(pid, observed.get());
        assert_eq!(cause, OneShotObservedFailure::Setup);
        assert_eq!(setups.get(), 1);
        assert_eq!(sup.ledger()[0].pid, pid);
        if cleanup.reaped || sup.ledger()[0].reaped {
            assert!(released.load(Ordering::SeqCst));
            assert_eq!(sup.stats().workers_live, 0);
        } else {
            // No false cleanup success: the owning supervisor must retain admission and keepalive.
            assert!(!released.load(Ordering::SeqCst));
            assert!(sup.admit(0).is_err());
        }
        assert_eq!(sup.retry_cleanup(), 0);
        assert!(sup.ledger()[0].reaped);
        assert!(released.load(Ordering::SeqCst));
        assert!(sup.admit(0).is_ok());
    }
}
