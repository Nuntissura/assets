//! Optional JavaScript expression provider; isolate C/FFI/toolchain/runtime quotas from pure Motion
//! semantics. Shared host sandbox lifecycle; no private script/plugin authority.
// No `unsafe` here: the Windows Job Object FFI lives in `hsk-studio-worker`.
#![forbid(unsafe_code)]

mod clock;
mod descriptor;
mod engine;
mod limits;
#[cfg(feature = "quickjs")]
mod os_caps;
mod profile;
mod provider;
#[cfg(feature = "quickjs")]
mod quickjs;
#[cfg(feature = "quickjs")]
mod quickjs_wire;
#[cfg(feature = "quickjs")]
mod quickjs_worker;
mod scripted;

pub use clock::{Clock, ManualClock, SystemClock};
pub use descriptor::DESCRIPTOR;
pub use engine::{
    BudgetKind, Engine, EngineFailure, EngineFault, EngineIdentity, EngineLimits, EngineRun,
    EvalJob, HostBridge, JsValue, RealmReceipt, SourceLanguage, ThrownKind,
};
pub use limits::{ConfigError, QuotaSpec, Quotas};
#[cfg(feature = "quickjs")]
pub use os_caps::{
    DEFAULT_CPU_SLACK, DEFAULT_MEMORY_SLACK_BYTES, MIN_PROCESS_MEMORY_BYTES, OsCapConfig, OsCapPolicy,
    OsCapStatus, OsCapUnavailable, OsLimits,
};
pub use profile::{
    DEFAULT_MAX_DEPTH, DEFAULT_MAX_TOKENS, DENIED_KEYWORDS, DENIED_NAMES, MAX_DEPTH_CEILING,
    ProfileLimits, ProfileValidator, ProfileViolation, ValidatedSource, ViolationClass,
    ViolationKind,
};
pub use provider::{AE_GLOBALS_PRELUDE, MotionJsProvider, PROFILE_ID, ProviderStats, RetireReport, report_eval};
#[cfg(feature = "quickjs")]
pub use quickjs::{
    LimitReadback, QuickJsBackend, QuickJsConfigError, QuickJsOptions, WRAPPER_STACK_LIMIT,
};
/// Worker fault injection for this crate's isolation tests (non-default `test-faults` feature only).
#[cfg(all(feature = "quickjs", feature = "test-faults"))]
pub use quickjs::WorkerTestFault;
#[cfg(feature = "quickjs")]
pub use quickjs_worker::{WORKER_PROGRAM, WorkerConfig, WorkerStats, worker_main};
pub use scripted::ScriptedEngine;
