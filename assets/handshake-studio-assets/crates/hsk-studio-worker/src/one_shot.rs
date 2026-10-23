use crate::{
    one_shot_api::{ExactLaunch, OneShotEvent, OneShotReason, OneShotRequest, OneShotStartError,
        OneShotTerminal, OutputBounds, OutputStream, OneShotObservedFailure, OneShotObservedStartError},
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
        let mut process = self.adopt(child, permit, request.owner_session, request.keepalive);
        let pid = process.pid();
        let observation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| observer(pid)));
        let failure = match observation {
            Ok(Ok(())) => None,
            Ok(Err(())) => Some(OneShotObservedFailure::ObserverRejected),
            Err(payload) => {
                // Establish child custody before propagating the opaque payload without dropping it.
                let _ = process.terminate(Duration::ZERO);
                drop(process);
                std::panic::resume_unwind(payload)
            }
        };
        if let Some(cause) = failure {
            return Err(observed_failure(process, cause));
        }
        let (stdout, stderr) = match setup(pipes, &mut process) {
            Ok(readers) => readers,
            Err(_) => return Err(observed_failure(process, OneShotObservedFailure::Setup)),
        };
        Ok(OneShotSession {
            stdout: Some(stdout), stderr: Some(stderr), process: Some(process), cancel, deadline,
            bounds: request.bounds, stdout_bytes: 0, stderr_bytes: 0, stderr_first: false, pid,
        })
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
        let Some(pipe) = reader.as_mut() else { return Ok(None); };
        let available = limit.saturating_sub(*used);
        let length = self.bounds.chunk_bytes.get().min(
            usize::try_from(available.saturating_add(1)).unwrap_or(usize::MAX));
        let mut bytes = vec![0; length];
        match pipe.read(&mut bytes) {
            Ok(0) => { *reader = None; Ok(None) }
            Ok(count) => {
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
            if self.stdout.is_none() && self.stderr.is_none() {
                let status = self.process.as_mut().and_then(ProcessOwner::child_mut).map(|child| child.try_wait());
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
