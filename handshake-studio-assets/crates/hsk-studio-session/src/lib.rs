//! Headless single-document operation orchestration, provider registration and test ports. Multi-
//! output delivery captures immutable revision/presets/output verification and uses existing Job
//! Runtime via a port for queue/watchfolder/cancel/retry, without a second job database or queue
//! authority. No built-in all-engine mega dependency.
#![forbid(unsafe_code)]
