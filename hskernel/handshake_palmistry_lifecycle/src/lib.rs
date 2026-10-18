//! The LIFECYCLE INVERSION (MT-089, the HARD §6.13.3 requirement).
//!
//! A normal child process dies with its parent. Palmistry must do the OPPOSITE: it must SURVIVE the
//! parent's death so it can record + persist the evidence precisely when Handshake freezes or crashes
//! (§6.13.3: "A watcher that dies with its parent cannot record the parent's death"). Concretely:
//!
//! 1. **Launched WITH Handshake** — the spawn is MT-094's job; this MT defines the binary it spawns.
//! 2. **Closes ONLY on an explicit `Shutdown`** — not on parent death, not on a timer, not on EOF of
//!    the control socket.
//! 3. **Survives parent death** — it holds the parent's OS process handle and WAITS on it. An
//!    unexpected parent exit BEFORE a `Shutdown` is RECORDED (a crash/abnormal-exit signal MT-092
//!    consumes) and Palmistry KEEPS RUNNING; it MAY exit only after a bounded post-death finalize
//!    window (so the capture is never lost), or when an explicit `Shutdown` then arrives.
//! 4. **NOT kill-on-job-close** — it must never be added to a Win32 Job Object that terminates children
//!    on parent death. That is a CONTRACT ON THE MT-094 LAUNCHER (documented in [`JOB_OBJECT_CONTRACT`]);
//!    the watcher itself assumes no job membership.
//!
//! # The `ParentWatch` seam
//!
//! The parent-handle wait is behind the [`ParentWatch`] trait so the Windows `OpenProcess` /
//! `WaitForSingleObject` / `GetExitCodeProcess` implementation is isolated and the lifecycle state
//! machine is testable with a fake watch. The proof (AC-009-4) runs the REAL Windows watch against a
//! REAL hard-killed dummy parent — the fake exists only for fast unit coverage of the state machine.

#[cfg(windows)]
mod windows_parent_watch;
#[cfg(windows)]
pub use windows_parent_watch::WindowsParentWatch;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// A HARD contract on the MT-094 Handshake-side launcher, kept here as a machine-greppable constant so
/// a reviewer (AC-009-5) and the launcher author both see it. Palmistry MUST NOT be added to a Win32
/// Job Object configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (or any process-group that
/// terminates children on parent death). Doing so would kill the watcher at the instant of the parent's
/// death — the exact moment it must survive to record it (§6.13.3 spec defect). This crate links NO job
/// API and makes NO job-membership assumption; the survives-parent-death test (AC-009-4) is the
/// behavioral proof that the watcher does not die with its parent.
pub const JOB_OBJECT_CONTRACT: &str =
    "MT-094 launcher MUST NOT add palmistry to a Win32 Job Object with \
     JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE; the watcher must outlive + record the parent's death (\u{a7}6.13.3).";

/// How the parent's process handle resolved when the watch signalled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentExit {
    /// The parent exited with this OS exit code. Whether this is "clean" or "abnormal" is decided by
    /// the lifecycle (a Shutdown received FIRST => clean, regardless of code), not by the code alone.
    Exited { code: u32 },
    /// The parent handle could not be observed (e.g. the pid was already gone / access denied at open).
    /// Treated as an abnormal disappearance.
    Vanished,
}

/// The seam over the OS parent-handle wait. `wait_for_parent_exit` BLOCKS until the parent process
/// exits (or the per-call `timeout` elapses, returning `None` so the caller can re-check its run flag),
/// then yields how it exited. Implemented for real on Windows by [`WindowsParentWatch`]; faked in tests.
pub trait ParentWatch: Send {
    /// Block up to `timeout` for the watched parent to exit. `Some(exit)` if it exited within the
    /// window; `None` if the timeout elapsed and the parent is still alive (poll again).
    fn wait_for_parent_exit(&self, timeout: Duration) -> Option<ParentExit>;
}

/// Why the watcher's run loop ended — the terminal outcome, written into the survivor record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason")]
pub enum ExitReason {
    /// An explicit `Shutdown` control message ended the watcher cleanly. NO crash recorded.
    CleanShutdown,
    /// The parent died abnormally (no `Shutdown` first); the watcher SURVIVED, recorded it, ran the
    /// bounded post-death finalize, and is now exiting. `parent_exit_code` is the OS code if known.
    ParentDiedAbnormally { parent_exit_code: Option<u32> },
    /// A `Shutdown` arrived AFTER the parent had already died abnormally — the watcher recorded the
    /// abnormal exit, stayed alive through the finalize, and then exited on the explicit command.
    ShutdownAfterParentDeath { parent_exit_code: Option<u32> },
}

/// The survivor record Palmistry persists when its run loop ends. This is the durable evidence of what
/// it observed. MT-092/MT-093 enrich it (minidump path, FR forward); MT-089 writes the lifecycle facts.
/// Carries NO project/sensitive text — only the session id (an opaque token), pids, codes, timestamps,
/// and the typed reason — consistent with the substrate's typed-allowlist stance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurvivorRecord {
    /// The diagnostic session id (opaque token; not content).
    pub session_id: String,
    /// The watched parent pid.
    pub parent_pid: u32,
    /// Whether an abnormal parent death was observed (the signal MT-092 consumes).
    pub abnormal_parent_exit: bool,
    /// The parent OS exit code, if the handle resolved to a concrete code.
    pub parent_exit_code: Option<u32>,
    /// Whether an explicit `Shutdown` was received at any point.
    pub shutdown_received: bool,
    /// The terminal reason the run loop ended.
    pub exit_reason: ExitReason,
    /// Monotonic-ish wall-clock millis since the UNIX epoch when the record was written (best-effort;
    /// a numeric timestamp, not content).
    pub recorded_at_unix_ms: u128,
}

/// Shared, thread-safe lifecycle state. The control thread sets `shutdown_requested`; the parent-watch
/// thread sets the parent-death fields; the main run loop reads them to make the exit decision. All
/// flags are atomics / a small mutexed cell so no lock is held across a blocking wait.
#[derive(Default)]
pub struct LifecycleState {
    /// Set true by the control thread when a `Shutdown` message is handled.
    shutdown_requested: AtomicBool,
    /// Set true by the parent-watch thread when the parent exits.
    parent_exited: AtomicBool,
    /// Set true when the parent exit was abnormal (no shutdown had been requested at the moment of
    /// death). This is the typed parent-died signal AC-009-4 requires to be RECORDED.
    parent_exit_abnormal: AtomicBool,
    /// The parent OS exit code (if the handle resolved one). Guarded by a mutex because it is a u32
    /// option set once; reads are cheap and rare.
    parent_exit_code: std::sync::Mutex<Option<u32>>,
}

impl LifecycleState {
    /// Fresh state (nothing observed yet).
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that an explicit `Shutdown` was requested (called from the control thread).
    pub fn request_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::SeqCst);
    }

    /// Whether an explicit `Shutdown` has been requested.
    pub fn shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::SeqCst)
    }

    /// Record an observed parent exit (called from the parent-watch thread). The abnormal/clean
    /// classification is decided HERE atomically against the current shutdown flag: if NO shutdown had
    /// been requested at the instant of death, it is abnormal — the §6.13 clean-shutdown rule. This is
    /// the crash signal MT-092 consumes.
    pub fn record_parent_exit(&self, exit: ParentExit) {
        let code = match exit {
            ParentExit::Exited { code } => Some(code),
            ParentExit::Vanished => None,
        };
        *self
            .parent_exit_code
            .lock()
            .expect("parent_exit_code mutex") = code;
        // Classify against the shutdown flag AT THE MOMENT OF DEATH (AC-009-6: a Shutdown that preceded
        // the parent exit means clean, even though the parent then exits).
        let abnormal = !self.shutdown_requested.load(Ordering::SeqCst);
        self.parent_exit_abnormal.store(abnormal, Ordering::SeqCst);
        self.parent_exited.store(true, Ordering::SeqCst);
    }

    /// Whether the parent has exited.
    pub fn parent_exited(&self) -> bool {
        self.parent_exited.load(Ordering::SeqCst)
    }

    /// Whether the observed parent exit was abnormal (the recorded crash signal).
    pub fn parent_exit_abnormal(&self) -> bool {
        self.parent_exit_abnormal.load(Ordering::SeqCst)
    }

    /// The parent OS exit code if one was resolved.
    pub fn parent_exit_code(&self) -> Option<u32> {
        *self
            .parent_exit_code
            .lock()
            .expect("parent_exit_code mutex")
    }
}

/// Tuning knobs for the run loop. Small enough to keep the proof fast while still being realistic.
#[derive(Debug, Clone, Copy)]
pub struct LifecycleConfig {
    /// How long the run loop sleeps between checks of the shared flags.
    pub poll_interval: Duration,
    /// The bounded post-death FINALIZE window: after an abnormal parent death, the watcher stays alive
    /// at least this long to capture/persist (MT-092/013 fill the capture) before it MAY exit. It must
    /// NEVER exit at the instant of death. An explicit `Shutdown` during the window ends it early.
    pub post_death_finalize: Duration,
    /// How long each blocking parent-watch wait call blocks before returning `None` to re-check the run
    /// flag. Bounds responsiveness to a clean shutdown while the parent is still alive.
    pub parent_watch_slice: Duration,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_millis(20),
            post_death_finalize: Duration::from_millis(500),
            parent_watch_slice: Duration::from_millis(100),
        }
    }
}

/// Spawn the parent-watch thread: it repeatedly blocks on `watch.wait_for_parent_exit(slice)` until the
/// parent exits, then records the exit into the shared state and returns. The slice bound lets the
/// thread also stop promptly once a shutdown is in progress (it re-checks `run`). Returns the join
/// handle so the caller can join it on exit.
pub fn spawn_parent_watch(
    watch: Box<dyn ParentWatch>,
    state: Arc<LifecycleState>,
    run: Arc<AtomicBool>,
    slice: Duration,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while run.load(Ordering::SeqCst) {
            match watch.wait_for_parent_exit(slice) {
                Some(exit) => {
                    state.record_parent_exit(exit);
                    return;
                }
                None => {
                    // Parent still alive; loop and re-check `run` so a clean shutdown stops this thread.
                    continue;
                }
            }
        }
    })
}

/// Run the lifecycle to completion given the shared `state` (fed by the control thread + the parent
/// watch thread) and the timing `config`. Returns the terminal [`ExitReason`]. This is the heart of the
/// inversion:
///
/// - It loops, sleeping `poll_interval`, observing the shared flags.
/// - If an explicit `Shutdown` is seen while the parent is still alive => `CleanShutdown` (prompt exit,
///   NO crash).
/// - If the parent dies and it was ABNORMAL (no prior shutdown) => it does NOT exit at the instant of
///   death; it enters the bounded `post_death_finalize` window (SURVIVING the parent), then exits with
///   `ParentDiedAbnormally` — UNLESS an explicit `Shutdown` arrives during the window, which yields
///   `ShutdownAfterParentDeath`.
/// - If the parent exit was classified clean (a `Shutdown` preceded it) => `CleanShutdown`.
///
/// The `run` flag is cleared on exit so the parent-watch thread can stop.
pub fn run_lifecycle(
    state: &LifecycleState,
    run: &AtomicBool,
    config: LifecycleConfig,
) -> ExitReason {
    let reason = loop {
        // 1) Explicit clean shutdown while the parent is still alive: exit promptly, no crash.
        if state.shutdown_requested() && !state.parent_exited() {
            break ExitReason::CleanShutdown;
        }

        // 2) Parent has exited — decide clean vs abnormal.
        if state.parent_exited() {
            let code = state.parent_exit_code();
            if !state.parent_exit_abnormal() {
                // A Shutdown preceded the death => clean (AC-009-6: not a crash).
                break ExitReason::CleanShutdown;
            }
            // ABNORMAL parent death (AC-009-4): the watcher SURVIVED to here (it did not die with the
            // parent). Record is already set in `state`. Now run the bounded finalize — NEVER exit at
            // the instant of death (RISK-009-6) — and watch for a late explicit Shutdown.
            let finalize_deadline = Instant::now() + config.post_death_finalize;
            loop {
                if state.shutdown_requested() {
                    break;
                }
                if Instant::now() >= finalize_deadline {
                    break;
                }
                std::thread::sleep(config.poll_interval.min(Duration::from_millis(10)));
            }
            if state.shutdown_requested() {
                break ExitReason::ShutdownAfterParentDeath {
                    parent_exit_code: code,
                };
            }
            break ExitReason::ParentDiedAbnormally {
                parent_exit_code: code,
            };
        }

        std::thread::sleep(config.poll_interval);
    };
    run.store(false, Ordering::SeqCst);
    reason
}

/// Build the [`SurvivorRecord`] from the final state + reason. Pure assembly; the caller persists it.
pub fn build_survivor_record(
    session_id: &str,
    parent_pid: u32,
    state: &LifecycleState,
    reason: ExitReason,
) -> SurvivorRecord {
    let recorded_at_unix_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    SurvivorRecord {
        session_id: session_id.to_string(),
        parent_pid,
        abnormal_parent_exit: state.parent_exit_abnormal(),
        parent_exit_code: state.parent_exit_code(),
        shutdown_received: state.shutdown_requested(),
        exit_reason: reason,
        recorded_at_unix_ms,
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// A fake parent watch driven by a flag the test flips, plus an optional fixed exit. It blocks (by
    /// short sleeps) until the flag says the parent died, returning the configured exit. Lets the state
    /// machine be unit-tested deterministically without an OS process.
    struct FakeWatch {
        dead: Arc<AtomicBool>,
        exit: ParentExit,
    }

    impl ParentWatch for FakeWatch {
        fn wait_for_parent_exit(&self, timeout: Duration) -> Option<ParentExit> {
            let deadline = Instant::now() + timeout;
            while Instant::now() < deadline {
                if self.dead.load(Ordering::SeqCst) {
                    return Some(self.exit);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            None
        }
    }

    fn fast_config() -> LifecycleConfig {
        LifecycleConfig {
            poll_interval: Duration::from_millis(2),
            post_death_finalize: Duration::from_millis(60),
            parent_watch_slice: Duration::from_millis(20),
        }
    }

    #[test]
    fn clean_shutdown_while_parent_alive_records_no_crash() {
        let state = Arc::new(LifecycleState::new());
        let run = Arc::new(AtomicBool::new(true));
        let dead = Arc::new(AtomicBool::new(false));
        let _watch = spawn_parent_watch(
            Box::new(FakeWatch {
                dead: Arc::clone(&dead),
                exit: ParentExit::Exited { code: 0 },
            }),
            Arc::clone(&state),
            Arc::clone(&run),
            fast_config().parent_watch_slice,
        );

        // Request a clean shutdown; parent never dies.
        let s2 = Arc::clone(&state);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            s2.request_shutdown();
        });

        let reason = run_lifecycle(&state, &run, fast_config());
        assert_eq!(reason, ExitReason::CleanShutdown);
        assert!(
            !state.parent_exit_abnormal(),
            "clean shutdown must record no crash"
        );
        let rec = build_survivor_record("sess", 123, &state, reason);
        assert!(!rec.abnormal_parent_exit);
        assert!(rec.shutdown_received);
    }

    #[test]
    fn abnormal_parent_death_survives_and_records_then_finalizes() {
        let state = Arc::new(LifecycleState::new());
        let run = Arc::new(AtomicBool::new(true));
        let dead = Arc::new(AtomicBool::new(false));
        let _watch = spawn_parent_watch(
            Box::new(FakeWatch {
                dead: Arc::clone(&dead),
                exit: ParentExit::Exited { code: 0xDEAD },
            }),
            Arc::clone(&state),
            Arc::clone(&run),
            fast_config().parent_watch_slice,
        );

        // Kill the parent with NO prior shutdown.
        let d2 = Arc::clone(&dead);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(15));
            d2.store(true, Ordering::SeqCst);
        });

        let start = Instant::now();
        let reason = run_lifecycle(&state, &run, fast_config());
        let elapsed = start.elapsed();

        // It RECORDED the abnormal exit...
        assert!(
            state.parent_exit_abnormal(),
            "abnormal parent death must be recorded"
        );
        assert_eq!(state.parent_exit_code(), Some(0xDEAD));
        // ...survived past the instant of death (it did not exit immediately — finalize window held)...
        assert!(
            elapsed >= Duration::from_millis(60),
            "must hold the bounded finalize window, not exit at the instant of death (elapsed {elapsed:?})"
        );
        // ...and then exited with the abnormal reason.
        assert_eq!(
            reason,
            ExitReason::ParentDiedAbnormally {
                parent_exit_code: Some(0xDEAD)
            }
        );
    }

    #[test]
    fn shutdown_during_finalize_yields_shutdown_after_death() {
        let state = Arc::new(LifecycleState::new());
        let run = Arc::new(AtomicBool::new(true));
        let dead = Arc::new(AtomicBool::new(false));
        let _watch = spawn_parent_watch(
            Box::new(FakeWatch {
                dead: Arc::clone(&dead),
                exit: ParentExit::Exited { code: 1 },
            }),
            Arc::clone(&state),
            Arc::clone(&run),
            fast_config().parent_watch_slice,
        );

        let d2 = Arc::clone(&dead);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            d2.store(true, Ordering::SeqCst);
        });
        // Shutdown arrives DURING the finalize window (after death).
        let s2 = Arc::clone(&state);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            s2.request_shutdown();
        });

        let reason = run_lifecycle(&state, &run, fast_config());
        assert!(state.parent_exit_abnormal());
        assert_eq!(
            reason,
            ExitReason::ShutdownAfterParentDeath {
                parent_exit_code: Some(1)
            }
        );
    }

    #[test]
    fn job_object_contract_is_documented() {
        // AC-009-5: the no-kill-on-job-close contract is greppable + names the spec section.
        assert!(JOB_OBJECT_CONTRACT.contains("JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE"));
        assert!(JOB_OBJECT_CONTRACT.contains("6.13.3"));
    }
}
