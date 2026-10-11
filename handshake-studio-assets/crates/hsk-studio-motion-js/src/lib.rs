//! Optional JavaScript expression provider; isolate C/FFI/toolchain/runtime quotas from pure Motion
//! semantics. Shared host sandbox lifecycle; no private script/plugin authority.
#![forbid(unsafe_code)]

mod descriptor;
mod engine;
mod limits;
mod provider;
mod scripted;

pub use descriptor::DESCRIPTOR;
pub use engine::{
    BudgetKind, Engine, EngineFailure, EngineIdentity, EngineLimits, EngineRun, EvalJob,
    HostBridge, JsValue, RealmReceipt, ThrownKind,
};
pub use limits::{ConfigError, QuotaSpec, Quotas};
pub use provider::{
    Clock, ManualClock, MotionJsProvider, PROFILE_ID, ProviderStats, RetireReport, SystemClock,
    report_eval,
};
pub use scripted::ScriptedEngine;
