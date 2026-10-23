//! Operating-system caps of one worker process: the data shared by every consumer, and the non-Windows
//! (or feature-off) stand-in for the Job Object guard.
#![deny(clippy::arithmetic_side_effects)]
use std::time::Duration;

/// Caps applied to one worker process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OsLimits {
    /// Most bytes the worker process may commit (Windows: `JOB_OBJECT_LIMIT_PROCESS_MEMORY`).
    pub process_memory_bytes: u64,
    /// User-mode CPU time one evaluation attempt may use (re-armed before every request).
    pub cpu_time: Duration,
}

/// What the operating system reported about a worker since the last drain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OsEvents {
    /// The process (or job) hit its memory cap: a commit beyond the cap was refused.
    pub memory: bool,
    /// The process exceeded its CPU time cap and was terminated by the kernel.
    pub cpu: bool,
}

impl OsEvents {
    pub fn is_clear(self) -> bool {
        !self.memory && !self.cpu
    }

    pub fn merge(&mut self, other: Self) {
        self.memory |= other.memory;
        self.cpu |= other.cpu;
    }
}

/// Whether this build can put workers under operating-system caps (Windows with feature `job-caps`).
pub const JOB_CAPS_AVAILABLE: bool = cfg!(all(windows, feature = "job-caps"));

#[cfg(all(windows, feature = "job-caps"))]
pub use crate::job::{WorkerJob, harden_worker_process};

/// Worker-process start-up hardening against the platform crash reporter (Windows with `job-caps`).
#[cfg(not(all(windows, feature = "job-caps")))]
pub fn harden_worker_process() {}

/// Stand-in so the supervisor type-checks where no Job Object implementation is compiled; `attach`
/// always fails, so a session never holds one.
#[cfg(not(all(windows, feature = "job-caps")))]
pub struct WorkerJob;

#[cfg(not(all(windows, feature = "job-caps")))]
impl WorkerJob {
    pub fn attach(_child: &std::process::Child, _limits: OsLimits) -> std::io::Result<Self> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    pub fn arm_cpu(&mut self, _budget: Duration) -> std::io::Result<()> {
        Ok(())
    }

    pub fn cpu_exceeded(&self) -> bool {
        false
    }

    pub fn drain(&self) -> OsEvents {
        OsEvents::default()
    }
}
