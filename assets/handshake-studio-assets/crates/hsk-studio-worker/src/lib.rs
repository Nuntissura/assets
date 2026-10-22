//! Studio worker-process supervision: the single owner (Operator decision D18 option (a), 2026-10-14) of
//! starting, watching and reclaiming the isolated worker processes used by `hsk-studio-motion-js`,
//! `hsk-studio-reel-native` and `hsk-studio-score-plugin`.
//!
//! Owned here:
//! * spawn: a child process with an empty environment (`env_clear`), piped stdin/stdout, a null stderr and, on
//!   Windows, the `CREATE_NO_WINDOW` creation flag, so no console window opens and no focus is taken;
//! * worker-program lookup beside the running executable only, with an explicit override (D14);
//! * deadline and kill: a wall-clock deadline and a cancellation check per request (the HELLO wait is
//!   cancellable too); on expiry or cancel the child is killed and always reaped;
//! * liveness probe of a running worker ([`Session::heartbeat`]);
//! * crash classification: end of stream and exit code, protocol fault, deadline, cancel ([`Classified`]);
//! * Windows Job Object caps (feature `job-caps`): process memory, per-request CPU time, kill-on-close,
//!   one process; kernel events via [`Session::drain_os_events`];
//! * admission (worker and byte budget), a process ledger and counters, and a bounded respawn rate.
//!
//! Not owned: wire formats, request and reply meaning, and each consumer's error vocabulary. This crate has
//! no fault-injection hook; its supervision tests use the `hsk-worker-testbin` helper behind the
//! non-default `test-faults` feature.
// `unsafe` exists only in `job` (Windows Job Object FFI, feature `job-caps`); every other build forbids it.
#![cfg_attr(not(all(windows, feature = "job-caps")), forbid(unsafe_code))]
#![cfg_attr(all(windows, feature = "job-caps"), deny(unsafe_code))]

mod caps;
#[cfg(all(windows, feature = "job-caps"))]
#[allow(unsafe_code)]
mod job;
mod program;
mod supervisor;
mod time;

pub use caps::{JOB_CAPS_AVAILABLE, OsEvents, OsLimits, WorkerJob, harden_worker_process};
pub use program::{discover_program, find_beside, resolve_program};
pub use supervisor::{
    Classified, EXIT_PATIENCE, Failure, Liveness, Permit, ProcessLedgerEntry, ReadFailure, ReadFn,
    Refused, RespawnGate, Session, SpawnError, StartError, StartRequest, Supervisor,
    SupervisorConfig, SupervisorStats, TerminalFn, Terminated, WAIT_SLICE, Waited,
};
pub use time::{WAIT_CEILING, after};
