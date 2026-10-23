//! Studio worker-process supervision: the single owner (Operator decision D18 option (a), 2026-10-14) of
//! starting, watching and reclaiming the isolated worker processes used by `hsk-studio-motion-js`,
//! `hsk-studio-reel-native` and `hsk-studio-score-plugin`.
//!
//! Owned here:
//! * framed spawn: a child process with an empty environment (`env_clear`), piped stdin/stdout, a null stderr and, on
//!   Windows, the `CREATE_NO_WINDOW` creation flag, so no console window opens and no focus is taken;
//! * worker-program lookup beside the running executable only, with an explicit override (D14);
//! * deadline and kill: a wall-clock deadline and a cancellation check per request (the HELLO wait is
//!   cancellable too); expiry or cancel initiates kill and reap, with failed cleanup retained;
//! * liveness probe of a running worker ([`Session::heartbeat`]);
//! * crash classification: end of stream and exit code, protocol fault, deadline, cancel ([`Classified`]);
//! * Windows Job Object caps (feature `job-caps`): process memory, per-request CPU time, kill-on-close,
//!   one process; kernel events via [`Session::drain_os_events`];
//! * admission (worker and byte budget), a process ledger and counters, and a bounded respawn rate.
//!
//! Not owned: wire formats, request and reply meaning, and each consumer's error vocabulary. This crate has
//! no fault-injection hook; its supervision tests use the `hsk-worker-testbin` helper behind the
//! non-default `test-faults` feature.
//! One-shot callers use [`Supervisor::start_one_shot`] with exact native arguments, cwd and an empty
//! inherited environment. Pull output until the fused iterator emits its terminal outcome. Output
//! remains raw bytes with separate bounded stdout/stderr streams; Debug never includes payloads.
//! Retain the immutable caller plan for result attribution. This crate owns no profile, content hash,
//! authorization, storage or GUI policy. A `ReclamationPending` outcome requires
//! [`Supervisor::retry_cleanup`]; it is not proof of completed reclamation. Retain an accessible
//! [`Supervisor`] and retry cleanup until its pending count is zero. Pending children keep their
//! permits and keepalive resources; dropping all supervisor handles does not establish cleanup.
//!
//! Implementation basis: Rust [`std::process::Child`] requires explicit wait/reap, Windows
//! [PeekNamedPipe](https://learn.microsoft.com/windows/win32/api/namedpipeapi/nf-namedpipeapi-peeknamedpipe)
//! permits bounded pipe inspection, and Unix [`std::os::unix::net::UnixStream`] supports nonblocking
//! reads. Reuse this supervisor's admission, ledger and child ownership rather than host spawning.
#![deny(unsafe_code)]

mod caps;
#[cfg(all(windows, feature = "job-caps"))]
#[allow(unsafe_code)]
mod job;
mod program;
mod one_shot;
mod one_shot_api;
#[cfg_attr(windows, allow(unsafe_code))]
mod raw_pipe;
mod supervisor;
mod time;

pub use caps::{JOB_CAPS_AVAILABLE, OsEvents, OsLimits, WorkerJob, harden_worker_process};
pub use program::{discover_program, find_beside, resolve_program};
pub use one_shot::OneShotSession;
pub use one_shot_api::{ExactLaunch, OneShotEvent, OneShotReason, OneShotRequest, OneShotStartError,
    OneShotTerminal, OutputBounds, OutputStream};
pub use supervisor::{
    Classified, EXIT_PATIENCE, Failure, Liveness, Permit, ProcessLedgerEntry, ReadFailure, ReadFn,
    Refused, RespawnGate, Session, SpawnError, StartError, StartRequest, Supervisor,
    SupervisorConfig, SupervisorStats, TerminalFn, Terminated, WAIT_SLICE, Waited,
};
pub use time::{WAIT_CEILING, after};
