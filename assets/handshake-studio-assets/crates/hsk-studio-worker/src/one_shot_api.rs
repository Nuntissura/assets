//! Native launch inputs and bounded byte output for one-shot workers.
use crate::Terminated;
use std::{collections::BTreeMap, ffi::OsString, fmt, num::NonZeroUsize, path::Path};

/// Exact caller-owned inputs. The worker clears inherited environment and uses no shell.
pub struct ExactLaunch<'a> {
    pub executable: &'a Path,
    pub cwd: &'a Path,
    pub args: &'a [OsString],
    pub env: &'a BTreeMap<OsString, OsString>,
}

impl fmt::Debug for ExactLaunch<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ExactLaunch([redacted])")
    }
}

/// Output is never silently truncated. Exceeding a stream's total ends execution.
/// Chunk sizes above 65536 bytes are rejected before spawn.
#[derive(Clone, Copy, Debug)]
pub struct OutputBounds {
    pub chunk_bytes: NonZeroUsize,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

pub struct OneShotRequest<'a> {
    pub owner_session: &'a str,
    pub launch: ExactLaunch<'a>,
    pub bounds: OutputBounds,
    /// Kept until the child is confirmed reaped, including pending cleanup.
    pub keepalive: Option<Box<dyn Send>>,
}

impl fmt::Debug for OneShotRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OneShotRequest([redacted])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputStream { Stdout, Stderr }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OneShotReason {
    Exited { success: bool, code: Option<i32> },
    Canceled,
    Deadline,
    OutputLimit(OutputStream),
    Io,
    /// The child has not been confirmed reaped. The supervisor retains its admission.
    ReclamationPending,
}

/// An execution outcome is not cleanup proof unless `cleanup.reaped` is true.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OneShotTerminal {
    pub reason: OneShotReason,
    pub cleanup: Terminated,
}

pub enum OneShotEvent {
    Output { stream: OutputStream, bytes: Vec<u8> },
    Terminal(OneShotTerminal),
}

impl fmt::Debug for OneShotEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Output { stream, bytes } => f.debug_struct("Output")
                .field("stream", stream).field("byte_count", &bytes.len()).finish(),
            Self::Terminal(outcome) => f.debug_tuple("Terminal").field(outcome).finish(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OneShotStartError {
    Canceled,
    Deadline,
    UnsupportedPlatform,
    UnsupportedCaps,
    InvalidLaunch,
    NotStarted,
    Setup(Terminated),
}

/// A returned failure after a real child was adopted. Observer panics resume after owned cleanup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OneShotObservedFailure {
    ObserverRejected,
    Setup,
}

/// Distinguishes refusal before spawn from a real child whose cleanup was attempted.
/// If cleanup is pending, retain the supervisor and use its cleanup retry API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OneShotObservedStartError {
    BeforeSpawn(OneShotStartError),
    AfterSpawn { pid: u32, cause: OneShotObservedFailure, cleanup: Terminated },
}

/// Owning process supplied by a creation-time isolation adapter. Implementations retain all tree,
/// cleanup and output authority until the supervisor confirms complete reclamation.
/// Methods must not panic. In particular cleanup must remain usable during another unwind.
pub trait OneShotProcess: Send {
    fn pid(&self) -> u32;
    /// Observe exit without releasing tree or cleanup authority.
    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>>;
    /// Bounded and repeatable. `reaped` is true only after OS reap AND required resource cleanup.
    /// On failure retain all remaining authority in this object for another attempt.
    fn terminate(&mut self, patience: std::time::Duration) -> Terminated;
    /// Nonblocking, sole-reader access: 0 is EOF, WouldBlock means no bytes yet. Never return more
    /// than buffer.len(). Both streams must obey this contract, including after process exit.
    fn try_read(&mut self, stream: OutputStream, buffer: &mut [u8]) -> std::io::Result<usize>;
}

/// BeforeSpawn promises no OS child was created. A post-creation failure transfers the genuine
/// owning process, even when startup failed or it already exited. Never discard its cleanup custody.
pub enum OneShotLaunchError {
    BeforeSpawn,
    AfterSpawn(Box<dyn OneShotProcess>),
}

impl fmt::Debug for OneShotLaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BeforeSpawn => f.write_str("BeforeSpawn"),
            Self::AfterSpawn(_) => f.write_str("AfterSpawn([owned process])"),
        }
    }
}
