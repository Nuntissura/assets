//! Studio worker-process supervision: the single owner (Operator decision D18 option (a), 2026-10-14) of
//! starting, watching and reclaiming the isolated worker processes used by `hsk-studio-motion-js`,
//! `hsk-studio-reel-native` and `hsk-studio-score-plugin`.
//!
//! Owned here (moved in from the three consumers by a later lane; this crate is a skeleton until then):
//! * spawn: a child process with an empty environment (`env_clear`), piped stdin/stdout, a null stderr and, on
//!   Windows, the `CREATE_NO_WINDOW` creation flag, so no console window opens and no focus is taken;
//! * deadline and kill: a wall-clock deadline and a cancellation check per request; on expiry or cancel the
//!   child is killed and always reaped;
//! * heartbeat and liveness of a running worker;
//! * crash classification: end of stream, exit code, protocol fault, deadline, cancel.
//!
//! Not owned: wire formats, request and reply meaning, and each consumer's error vocabulary.
//! Worker executables ship beside the host executable; a consumer may point at another program (D14).
