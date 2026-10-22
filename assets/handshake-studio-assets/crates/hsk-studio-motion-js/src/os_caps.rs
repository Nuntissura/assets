//! Operating-system caps for the QuickJS worker process (cargo feature `quickjs`).
//!
//! Why: every other budget class is decided by state the engine or the script can influence (the
//! interrupt handler, a catchable error object). A cap enforced by the operating system on the worker
//! process, and observed by the HOST through a kernel notification, cannot be forged by script text:
//! only a real allocation beyond the cap, or real CPU time beyond the cap, produces the event.
//!
//! Windows: one Job Object per worker (`hsk-studio-worker` job module): process memory limit (committed bytes), per
//! process user CPU time limit (re-armed before every request, because the kernel counts the whole
//! process lifetime; the host also polls the job's own CPU accounting every few milliseconds because
//! the kernel limit alone was measured to stop a runaway loop seconds late), kill-on-close,
//! die-on-unhandled-exception (no crash-reporter hold), and at most one process. The host drains the job's completion port after every request and maps
//! `JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT` / `JOB_OBJECT_MSG_JOB_MEMORY_LIMIT` to `Budget(Memory)` and
//! `JOB_OBJECT_MSG_END_OF_PROCESS_TIME` to `Budget(Cpu)`. Microsoft documents completion-port
//! messages as notifications whose delivery is not guaranteed, so a missed message degrades to the
//! previous classification (thrown error or crash), never to a false budget denial.
//!
//! Other platforms: typed `OsCapStatus::Unavailable(PlatformNotSupported)`; `OsCapPolicy::Required`
//! turns that into a configuration error. Nothing is emulated.
#![deny(clippy::arithmetic_side_effects)]
use crate::{limits::Quotas, quickjs::QuickJsConfigError};
pub use hsk_studio_worker::OsLimits;
use std::time::Duration;

/// Added to the engine heap and stack limits to get the default process memory cap: covers the Rust
/// runtime, the sized realm thread, the wire buffers and allocator overhead outside the engine heap.
pub const DEFAULT_MEMORY_SLACK_BYTES: u64 = 128 << 20;
/// Added to `deadline + kill_grace` to get the default CPU cap: covers process start-up and the
/// time between a missed cooperative poll and the host's wall-clock kill.
pub const DEFAULT_CPU_SLACK: Duration = Duration::from_millis(500);
/// A cap below this cannot hold the worker's own start-up footprint; rejected at configuration.
pub const MIN_PROCESS_MEMORY_BYTES: u64 = 32 << 20;

/// What to do when the platform cannot enforce the caps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OsCapPolicy {
    /// Enforce where the platform supports it; report `OsCapStatus::Unavailable` elsewhere.
    #[default]
    WhereAvailable,
    /// Refuse to build the backend on a platform without caps (`QuickJsConfigError::OsCapsUnavailable`).
    Required,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsCapUnavailable {
    /// No implementation for this operating system.
    PlatformNotSupported,
    /// The engine runs inside the caller's process (test-only `unsafe_in_process`): no worker to cap.
    InProcessEngine,
}

impl OsCapUnavailable {
    pub const fn code(self) -> &'static str {
        match self {
            Self::PlatformNotSupported => "os_caps_platform_not_supported",
            Self::InProcessEngine => "os_caps_in_process_engine",
        }
    }
}

/// Whether the worker processes of a backend run under operating-system caps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsCapStatus {
    Enforced,
    Unavailable(OsCapUnavailable),
}

/// Cap policy of a worker pool: derived from the quotas unless `limits` is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OsCapConfig {
    pub policy: OsCapPolicy,
    pub memory_slack_bytes: u64,
    pub cpu_slack: Duration,
    /// Explicit caps replacing the derived ones (operator tuning, tests that need a cap below the
    /// engine heap limit).
    pub limits: Option<OsLimits>,
}

impl Default for OsCapConfig {
    fn default() -> Self {
        Self {
            policy: OsCapPolicy::default(),
            memory_slack_bytes: DEFAULT_MEMORY_SLACK_BYTES,
            cpu_slack: DEFAULT_CPU_SLACK,
            limits: None,
        }
    }
}

impl OsCapConfig {
    /// Fail-closed resolution: explicit limits must be nonzero and hold the worker's footprint; the
    /// derived caps saturate instead of overflowing.
    pub(crate) fn resolve(
        &self,
        quotas: &Quotas,
        kill_grace: Duration,
    ) -> Result<OsLimits, QuickJsConfigError> {
        if let Some(limits) = self.limits {
            if limits.process_memory_bytes < MIN_PROCESS_MEMORY_BYTES || limits.cpu_time.is_zero() {
                return Err(QuickJsConfigError::WorkerPolicyInvalid);
            }
            return Ok(limits);
        }
        let memory = quotas
            .memory_bytes()
            .saturating_add(quotas.stack_bytes())
            .saturating_add(self.memory_slack_bytes)
            .max(MIN_PROCESS_MEMORY_BYTES);
        let cpu_time = quotas
            .deadline()
            .saturating_add(kill_grace)
            .saturating_add(self.cpu_slack);
        Ok(OsLimits {
            process_memory_bytes: memory,
            cpu_time,
        })
    }
}

pub(crate) const fn platform_status() -> OsCapStatus {
    if hsk_studio_worker::JOB_CAPS_AVAILABLE {
        OsCapStatus::Enforced
    } else {
        OsCapStatus::Unavailable(OsCapUnavailable::PlatformNotSupported)
    }
}
