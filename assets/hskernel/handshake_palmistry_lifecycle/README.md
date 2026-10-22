# Palmistry lifecycle

`handshake-palmistry-lifecycle` contains the existing watcher lifecycle state machine and Windows parent-process watch. It records an unexpected parent exit and continues through a bounded finalization window. An explicit shutdown can end that window early.

## Public API and use

1. Create shared `Arc<LifecycleState>` and `Arc<AtomicBool>` run state, initially true.
2. Supply a `ParentWatch` and call `spawn_parent_watch` with the configured wait slice. The returned thread handle belongs to the caller.
3. Route an explicit control shutdown to `LifecycleState::request_shutdown`. Parent-exit observations enter through `record_parent_exit`.
4. Call blocking `run_lifecycle` with `LifecycleConfig`. It returns `CleanShutdown`, `ParentDiedAbnormally`, or `ShutdownAfterParentDeath` and clears the run flag.
5. Join the watch thread and call `build_survivor_record`. The caller persists the returned `SurvivorRecord`; this library performs no persistence, capture, process launch, or control-socket handling.

`ParentExit` carries an OS exit code or `Vanished`. `SurvivorRecord` is serde-serializable and includes session ID, parent PID, observed flags, exit reason and a best-effort UNIX timestamp. Configuration defaults are a 20 ms poll interval, 500 ms finalization window and 100 ms parent-watch slice.

## Platform and dependencies

The state machine uses Rust standard-library synchronization and serde 1 with derive. Under `cfg(windows)`, `WindowsParentWatch::open(pid)` returns an optional process watch backed by a Windows handle. It needs windows-sys 0.61 Foundation and Threading APIs. That watch and its export are absent on other targets; callers can supply another `ParentWatch` implementation.

The launcher must honor `JOB_OBJECT_CONTRACT`: do not put the watcher in a job configured with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. This library neither launches the watcher nor establishes its job membership.

## Limits and recovery

`run_lifecycle` blocks the calling thread. A custom watch must respect its timeout so shutdown and joining remain bounded. If opening the Windows parent fails, the caller must handle `None`; the library does not automatically construct a replacement watch. Lifecycle mutex poisoning can panic through existing `expect` calls. Record persistence, capture failures and restart policy remain caller responsibilities.

Four retained unit tests cover the state machine with a fake watch and the launcher-contract text. Their presence is not proof of actual Windows parent-death survival, launch behavior, persistence or host integration. No test execution or runtime acceptance is claimed by this source placement. Original MT-089, MT-094 and host acceptance requirements remain outstanding at their owning proof boundaries.
