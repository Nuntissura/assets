use crate::{
    one_shot_api::{ExactLaunch, OneShotEvent, OneShotReason, OneShotRequest, OneShotStartError,
        OneShotTerminal, OutputBounds, OutputStream},
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
        if cancel.check().is_err() { return Err(OneShotStartError::Canceled); }
        if Instant::now() >= deadline { return Err(OneShotStartError::Deadline); }
        if !cfg!(any(unix, windows)) { return Err(OneShotStartError::UnsupportedPlatform); }
        if self.requests_os_caps() { return Err(OneShotStartError::UnsupportedCaps); }
        if !self.owns_permit(&permit) || request.bounds.chunk_bytes.get() > 65_536
            || !valid_launch(&request.launch) {
            return Err(OneShotStartError::InvalidLaunch);
        }
        let mut command = Command::new(request.launch.executable);
        command.args(request.launch.args).current_dir(request.launch.cwd)
            .env_clear().envs(request.launch.env).stdin(Stdio::null());
        quiet(&mut command);
        let pipes = Pipes::prepare(&mut command).map_err(|_| OneShotStartError::NotStarted)?;
        // Recheck after command/pipe setup, immediately before the only spawn boundary.
        if cancel.check().is_err() { return Err(OneShotStartError::Canceled); }
        if Instant::now() >= deadline { return Err(OneShotStartError::Deadline); }
        let child = command.spawn().map_err(|_| OneShotStartError::NotStarted)?;
        let mut process = self.adopt(child, permit, request.owner_session, request.keepalive);
        let readers = match process.child_mut() {
            Some(child) => pipes.readers(child),
            None => Err(io::ErrorKind::BrokenPipe.into()),
        };
        let (stdout, stderr) = match readers {
            Ok(readers) => readers,
            Err(_) => return Err(OneShotStartError::Setup(process.terminate(Duration::ZERO))),
        };
        let pid = process.pid();
        Ok(OneShotSession {
            stdout: Some(stdout), stderr: Some(stderr), process: Some(process), cancel, deadline,
            bounds: request.bounds, stdout_bytes: 0, stderr_bytes: 0, stderr_first: false, pid,
        })
    }
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
