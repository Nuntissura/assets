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
mod scripted;

pub use clock::{Clock, ManualClock, SystemClock};
pub use descriptor::DESCRIPTOR;
pub use engine::{
    BudgetKind, Engine, EngineFailure, EngineIdentity, EngineLimits, EngineRun, EvalJob,
    HostBridge, JsValue, RealmReceipt, ThrownKind,
};
pub use limits::{ConfigError, QuotaSpec, Quotas};
pub use provider::{MotionJsProvider, PROFILE_ID, ProviderStats, RetireReport, report_eval};
#[cfg(feature = "quickjs")]
pub use quickjs::{LimitReadback, QuickJsConfigError, QuickJsEngine, WRAPPER_STACK_LIMIT};
pub use scripted::ScriptedEngine;
