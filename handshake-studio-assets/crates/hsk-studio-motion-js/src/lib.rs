//! Optional JavaScript expression provider; isolate C/FFI/toolchain/runtime quotas from pure Motion
//! semantics. Shared host sandbox lifecycle; no private script/plugin authority.
#![forbid(unsafe_code)]

mod clock;
mod descriptor;
mod engine;
mod limits;
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
    EvalJob, HostBridge, JsValue, RealmReceipt, ThrownKind,
};
pub use limits::{ConfigError, QuotaSpec, Quotas};
pub use provider::{MotionJsProvider, PROFILE_ID, ProviderStats, RetireReport, report_eval};
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
