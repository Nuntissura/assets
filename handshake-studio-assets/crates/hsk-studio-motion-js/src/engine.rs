//! Engine adapter contract. An engine (a future native or pure-Rust JS backend) implements
//! `Engine::run` for ONE evaluation in a FRESH realm that it drops before returning. No engine
//! type appears in any signature here; engines are reached only through these neutral types.
//!
//! Adapter obligations (checked by the provider where observable, otherwise by differential tests):
//! * nonzero memory/stack/recursion limits from `EngineLimits` are applied BEFORE parse/compile/eval;
//! * every admitted control-flow/function/callback path calls `HostBridge::charge` (shared Fuel);
//! * `Budget`, `Canceled` and `Host` failures abort the evaluation and can never be caught or
//!   swallowed by script code (`try/catch` must not observe them);
//! * ambient authority is absent: no filesystem/network/process/env, no `eval`/`Function`
//!   reachability, no `Math.random`, no ambient clock (use `HostBridge::utc_ms`), no async/timers;
//! * result conversion is bounded by `max_output_nodes`/`max_output_depth` and never runs getters,
//!   proxies or user coercions; anything else is `EngineFailure::Output`.
use crate::clock::Clock;
use hsk_studio_accord::CancellationToken;
use hsk_studio_motion::{EvalTick, ExprError, SourceSpan};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineIdentity {
    pub name: &'static str,
    pub version: &'static str,
    /// True when the backend links C/FFI (needs the worker-process isolation slice).
    pub native: bool,
}

/// Nonzero, validated limits (see `Quotas`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EngineLimits {
    pub memory_bytes: u64,
    pub stack_bytes: u64,
    pub recursion_limit: u32,
    pub max_output_nodes: usize,
    pub max_output_depth: usize,
}

/// Immutable inputs of one evaluation (MOT-255: revision + canonical tick + explicit seed).
/// `cancel`, `clock` and `deadline_ns` are owned handles so an engine may poll them from its own
/// `'static` interrupt callbacks and threads without borrowing the bridge.
#[derive(Clone, Debug)]
pub struct EvalJob<'a> {
    pub cancel: CancellationToken,
    pub clock: Arc<dyn Clock>,
    pub deadline_ns: u64,
    pub source: &'a str,
    pub dimension: usize,
    pub base: Option<&'a [f64]>,
    pub revision: u64,
    pub tick: EvalTick,
    pub seed: u64,
    pub limits: EngineLimits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetKind {
    Fuel,
    Memory,
    Stack,
    Recursion,
    Deadline,
    HostReads,
    SourceBytes,
    Realms,
    /// CPU time of a worker process, enforced by an operating-system cap (never by script text).
    Cpu,
}

impl BudgetKind {
    pub const COUNT: usize = 9;

    pub const fn index(self) -> usize {
        match self {
            Self::Fuel => 0,
            Self::Memory => 1,
            Self::Stack => 2,
            Self::Recursion => 3,
            Self::Deadline => 4,
            Self::HostReads => 5,
            Self::SourceBytes => 6,
            Self::Realms => 7,
            Self::Cpu => 8,
        }
    }

    pub const fn code(self) -> &'static str {
        match self {
            Self::Fuel => "fuel",
            Self::Memory => "memory",
            Self::Stack => "stack",
            Self::Recursion => "recursion",
            Self::Deadline => "deadline",
            Self::HostReads => "host_reads",
            Self::SourceBytes => "source_bytes",
            Self::Realms => "realms",
            Self::Cpu => "cpu",
        }
    }
}

/// What an engine accepts as `EvalJob::source`. The provider applies the admitted-syntax profile
/// validator (`ProfileValidator`) before evaluation only for JavaScript engines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceLanguage {
    /// ECMAScript-shaped source (the Studio expression profile).
    JavaScript,
    /// A non-JavaScript test-double command language (`ScriptedEngine`); never validated as JS.
    Scripted,
}

/// Typed isolation faults of an out-of-process engine (never produced by in-process engines).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineFault {
    /// The worker process died (abort, native crash, killed from outside) without a valid reply.
    Crashed,
    /// The worker answered with an invalid, mismatched or wrongly versioned protocol frame.
    Protocol,
    /// The worker process could not be started or did not complete its handshake.
    SpawnFailed,
    /// Starting another worker was refused by the bounded respawn rate.
    RespawnLimited,
}

impl EngineFault {
    pub const COUNT: usize = 4;

    pub const fn index(self) -> usize {
        match self {
            Self::Crashed => 0,
            Self::Protocol => 1,
            Self::SpawnFailed => 2,
            Self::RespawnLimited => 3,
        }
    }

    pub const fn code(self) -> &'static str {
        match self {
            Self::Crashed => "engine_crashed",
            Self::Protocol => "engine_protocol",
            Self::SpawnFailed => "engine_spawn_failed",
            Self::RespawnLimited => "engine_respawn_limited",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThrownKind {
    /// Undeclared identifier / unresolved reference.
    Reference,
    /// Any other thrown error (type, range, user throw).
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineFailure {
    Parse(Option<SourceSpan>),
    Unsupported(Option<SourceSpan>),
    Thrown {
        kind: ThrownKind,
        span: Option<SourceSpan>,
    },
    Budget(BudgetKind),
    /// Result not a plain bounded numeric value.
    Output,
    /// Host/bridge failure propagated verbatim (typed code preserved).
    Host(ExprError),
    Canceled,
    /// Isolation fault of an out-of-process engine; the provider stays usable (the next call may
    /// respawn the worker, subject to the bounded respawn rate).
    Faulted(EngineFault),
    /// Adapter invariant broken; the provider poisons itself (fail closed).
    Internal,
}

/// Bounded neutral result tree. Adapters build it with node/depth caps already enforced.
#[derive(Clone, Debug, PartialEq)]
pub enum JsValue {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    Text(String),
    Array(Vec<JsValue>),
    /// Any object-like value (never inspected, never converted).
    Object,
}

/// What the adapter proves about its realm for this evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RealmReceipt {
    /// The realm started empty (no state from earlier evaluations).
    pub fresh: bool,
    /// The realm and all its values were released before `run` returned.
    pub dropped: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EngineRun {
    pub outcome: Result<JsValue, EngineFailure>,
    pub realm: RealmReceipt,
}

/// Immutable host services visible to script code.
pub trait HostBridge {
    /// Reads another property (`tick = None` -> the canonical evaluation tick).
    fn read(&mut self, path: &str, tick: Option<EvalTick>) -> Result<Vec<f64>, EngineFailure>;
    /// Units left in the shared `Fuel` (an engine bounds its in-engine polling by this).
    fn fuel_remaining(&self) -> u64;
    /// Shared checkpoint: polls cancel/deadline, then charges the shared `Fuel`.
    fn charge(&mut self, units: u64) -> Result<(), EngineFailure>;
    /// Polls cancel/deadline without charging.
    fn checkpoint(&mut self) -> Result<(), EngineFailure>;
    /// Extends the evaluation deadline by `ns` of host-side setup time that is not script time
    /// (e.g. starting a worker process). The default ignores it (no deadline to extend).
    fn grant_time(&mut self, _ns: u64) {}
    /// Deterministic Date instant (MOT-256) for the canonical tick; the only clock script may see.
    fn utc_ms(&mut self) -> Result<i64, EngineFailure>;
}

pub trait Engine: Send + Sync {
    fn identity(&self) -> EngineIdentity;
    /// Source language this engine evaluates (default JavaScript: the profile validator applies).
    fn source_language(&self) -> SourceLanguage {
        SourceLanguage::JavaScript
    }
    /// One evaluation in a fresh realm that is dropped before returning.
    fn run(&self, job: &EvalJob<'_>, host: &mut dyn HostBridge) -> EngineRun;
}
