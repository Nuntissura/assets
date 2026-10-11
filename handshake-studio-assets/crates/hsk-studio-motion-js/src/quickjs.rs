//! QuickJS backend (cargo feature `quickjs`, default OFF): `rquickjs =0.14.0` over the bundled
//! QuickJS-NG 0.16.2 C engine. Implements the neutral `Engine` adapter; no rquickjs type appears in
//! any public signature of this crate.
//!
//! Isolation: the production path runs every evaluation in a separate `motion-js-worker` process
//! (`quickjs_worker`), because hostile source can abort the host (an `async` function in a context
//! without the Promise intrinsic trips a QuickJS GC assert). The realm code below runs inside that
//! worker; hosting it in the caller's process (`QuickJsOptions::unsafe_in_process`) and the worker
//! fault injection (`WorkerTestFault`) exist only with the non-default `test-faults` feature.
//!
//! Controls enforced by the realm code (adoption review EXPR-PROVIDER-xx):
//! * 02: explicit nonzero heap limit applied BEFORE any context/parse/eval, accepted value read back
//!   (`malloc_limit`) and a mismatch fails closed; stack limit applied before eval and rejected at
//!   configuration when the wrapper would map it to 0/unlimited (above 16 MiB). The wrapper offers no
//!   stack readback; the effect is proven by a runaway-recursion test, not by a getter.
//! * 03: no `allocator`/`rust-alloc` feature; the default C allocator accounting is what the limit meters.
//! * 04: interrupt = cooperative polling. One engine poll (about 10,000 interpreter branch/call
//!   steps) costs one unit of the SHARED Motion `Fuel`; cancel and the monotonic deadline are polled
//!   there too. The interrupt stays ARMED for the whole realm session, including result extraction
//!   and error classification, so no user-reachable code path runs unmetered. This is a
//!   bounded-cooperation budget, NOT semantic AST fuel (EXPR-PROVIDER-13 stays NOT_PROVEN); the
//!   worker process adds the hard wall-clock kill for parsing and long C builtins.
//! * 05 (partial): `eval` and the four function constructors (Function, GeneratorFunction,
//!   AsyncFunction, AsyncGeneratorFunction; the only `js_function_constructor` registrations in the
//!   bundled source) are replaced by non-configurable throwers before user source runs, and the
//!   direct-eval identity check (`ctx->eval_obj`) can no longer match. The context still carries the
//!   Eval intrinsic because compiling ANY source needs it: a separate trusted compile realm is NOT
//!   built (no safe wrapper API), so completeness is NOT_PROVEN.
//! * 06 (partial): contexts carry only base objects + Eval + Promise: no Date, Proxy, JSON, RegExp,
//!   typed arrays, loader, console or libc APIs; `Math.random` throws; user source is strict mode.
//!   The Promise intrinsic is present ONLY because async function syntax in a context without it
//!   leaks a GC object and trips QuickJS `assert(list_empty(&rt->gc_obj_list))` at runtime free
//!   (found by probe: `async function f(){}` aborts the process in debug builds). The global
//!   `Promise` binding is deleted and pending jobs are never executed, so promises are inert.
//! * 07: a fresh Runtime and context per run, dropped on the thread that created them.
//! * 10: results leave the realm only through a trusted extractor that uses captured intrinsics,
//!   own-data-property descriptors and null-prototype descriptor objects, so getters, proxies,
//!   prototype accessors and user coercion never run. Thrown values are described by a second
//!   trusted function with the same discipline: error kind comes from the identity of captured
//!   intrinsic prototypes, never from user-writable names; the stack text comes from the captured
//!   native `Error.prototype.stack` getter.
//!
//! Classification (STUDIO-R5, `diagnostics/motion-js-classify.md`): budget classes come only from
//! host-side state a script cannot set: our interrupt handler (`Tripped`: Fuel, Cancel, Deadline) and
//! the rquickjs error type (`Error::Allocation` -> Memory). No message, name or shape read from the
//! realm selects a class. QuickJS-NG reports its stack limit and heap limit as ordinary catchable
//! `RangeError`/`InternalError` objects that a script can construct byte for byte, so those are
//! `Thrown { Other }` like any thrown error (the limit still stops the script; test
//! `quickjs_forged_budget_messages_are_thrown_not_budget`).
//!
//! Host reads use deterministic miss/replay: the script runs in a fresh realm against an
//! evaluation-local cache; the first uncached `hostRead` records a miss, the run is discarded, the
//! host performs that single read on the caller thread (shared Fuel, quotas), and the script is
//! replayed. Every distinct (path, tick) is charged once per evaluation.
#![deny(clippy::arithmetic_side_effects)]
use crate::{
    clock::Clock,
    engine::{
        BudgetKind, Engine, EngineFailure, EngineIdentity, EngineRun, EvalJob, HostBridge, JsValue,
        RealmReceipt, ThrownKind,
    },
    limits::Quotas,
    quickjs_worker::{WorkerConfig, WorkerEngine, WorkerStats},
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_motion::{EvalTick, ExprError, ExprErrorCode, SourceSpan};
use rquickjs::{
    Array, Context, Ctx, Function, Runtime, Value,
    context::{EvalOptions, intrinsic},
    function::Opt,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};
#[cfg(feature = "test-faults")]
use std::thread;

/// rquickjs maps any stack limit above this to 0 (= unlimited); configuration must reject it.
pub const WRAPPER_STACK_LIMIT: usize = 1 << 24;
const MIN_STACK: usize = 64 << 10;
const MIN_MEMORY: usize = 1 << 20;
pub(crate) const THREAD_STACK_HEADROOM: usize = 1 << 20;
const MAX_REPLAYS: usize = 4096;
const FILENAME: &str = "expr";
pub(crate) const ENGINE_VERSION: &str = "quickjs-ng 0.16.2, rquickjs 0.14.0";

/// Trusted bootstrap: closes the dynamic-compile entry points, removes ambient randomness and
/// returns `[extract, describe]` (kept by Rust, unreachable from user code). Everything captured here
/// runs before user source, so later monkey-patching of intrinsics cannot reach it.
const BOOTSTRAP: &str = r#"(function (g) {
  "use strict";
  var def = Object.defineProperty;
  var getProto = Object.getPrototypeOf;
  var gopd = Object.getOwnPropertyDescriptor;
  var create = Object.create;
  var isArray = Array.isArray;
  var TypeErr = TypeError;
  var MARK = "motionjs-unavailable:";
  var uncurry = Function.prototype.bind.bind(Function.prototype.call);
  var slice = uncurry(String.prototype.slice);
  var stackDesc = gopd(Error.prototype, "stack");
  var stackOf = stackDesc !== undefined && typeof stackDesc.get === "function" ? uncurry(stackDesc.get) : null;
  var errorProtos = [
    SyntaxError.prototype, ReferenceError.prototype, RangeError.prototype, TypeError.prototype,
    typeof InternalError === "function" ? InternalError.prototype : null, Error.prototype
  ];
  function deny(obj, key, what) {
    var thrower = function () { throw new TypeErr(MARK + " " + what); };
    def(obj, key, { value: thrower, writable: false, enumerable: false, configurable: false });
  }
  function protoOf(make) {
    try { return getProto(make()); } catch (e) { return null; }
  }
  var protos = [
    Function.prototype,
    protoOf(function () { return function* () {}; }),
    protoOf(function () { return async function () {}; }),
    protoOf(function () { return async function* () {}; })
  ];
  for (var i = 0; i < protos.length; i++) {
    if (protos[i] !== null) { deny(protos[i], "constructor", "dynamic function constructor"); }
  }
  deny(g, "Function", "Function");
  deny(g, "eval", "eval");
  delete g.Promise;
  deny(Math, "random", "Math.random");
  function extract(v, max) {
    if (typeof v === "number") { return v; }
    if (!isArray(v)) { return undefined; }
    var n = v.length;
    if (typeof n !== "number" || n > max) { return undefined; }
    var out = [];
    var desc = create(null);
    desc.writable = true;
    desc.enumerable = true;
    desc.configurable = true;
    for (var k = 0; k < n; k++) {
      var d = gopd(v, k);
      if (d === undefined || gopd(d, "value") === undefined) { return undefined; }
      if (typeof d.value !== "number") { return undefined; }
      desc.value = d.value;
      def(out, k, desc);
    }
    return out;
  }
  function kindOf(e) {
    var p = getProto(e);
    for (var n = 0; n < 64 && p !== null; n++) {
      for (var j = 0; j < errorProtos.length; j++) {
        if (p === errorProtos[j]) { return j + 1; }
      }
      p = getProto(p);
    }
    return 0;
  }
  // Kind and stack text only: messages are user data and never select a class (STUDIO-R5).
  function describe(e) {
    if (typeof e !== "object" || e === null) { return [0, ""]; }
    var kind = kindOf(e);
    var stack = "";
    if (stackOf !== null && kind !== 0) {
      try {
        var s = stackOf(e);
        if (typeof s === "string") { stack = slice(s, 0, 1024); }
      } catch (x) {}
    }
    return [kind, stack];
  }
  return [extract, describe];
})(globalThis)"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickJsConfigError {
    /// The wrapper would silently treat this stack size as unlimited.
    StackAboveWrapperLimit,
    StackBelowMinimum,
    MemoryBelowMinimum,
    MemoryLimitUnrepresentable,
    /// No `motion-js-worker` executable at the configured path or next to the running executable.
    WorkerProgramMissing,
    /// Zero spawn budget, window, timeout or kill grace.
    WorkerPolicyInvalid,
}

impl QuickJsConfigError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::StackAboveWrapperLimit => "stack_above_wrapper_limit",
            Self::StackBelowMinimum => "stack_below_minimum",
            Self::MemoryBelowMinimum => "memory_below_minimum",
            Self::MemoryLimitUnrepresentable => "memory_limit_unrepresentable",
            Self::WorkerProgramMissing => "worker_program_missing",
            Self::WorkerPolicyInvalid => "worker_policy_invalid",
        }
    }
}

/// What the engine reported back after the limits were applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitReadback {
    /// `JSMemoryUsage::malloc_limit` read back from the live runtime.
    pub memory_limit: i64,
    /// Stack limit handed to the wrapper (no readback API exists).
    pub stack_applied: usize,
}

/// Fault injection for this crate's isolation tests, read by the worker process from the environment
/// variable named in `ENV`; it reproduces failure classes the hardened engine no longer triggers by
/// itself. Compiled only with the non-default `test-faults` feature (V4-04): a product worker never
/// reads the variable (`tests/worker_fault_env.rs`).
#[cfg(feature = "test-faults")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkerTestFault {
    #[default]
    None,
    /// Sources containing `exit_now` make the worker exit abruptly without replying (a cheap crash
    /// class that, unlike a native abort, is not held open by the platform crash reporter).
    ExitMidRequest,
    /// Sources containing `async` run in the unmitigated slice-2 context (no Promise intrinsic, no
    /// bootstrap): QuickJS asserts at runtime free and the process aborts.
    AsyncAbort,
    /// The interrupt handler is not installed, so only the process-level deadline can stop a script
    /// (models a script stuck in a long C builtin that never reaches an engine poll).
    DeafInterrupt,
}

#[cfg(feature = "test-faults")]
impl WorkerTestFault {
    pub(crate) const ENV: &'static str = "HSK_MOTION_JS_WORKER_TEST_FAULT";

    pub(crate) const fn env_value(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::ExitMidRequest => Some("exit_mid_request"),
            Self::AsyncAbort => Some("async_abort"),
            Self::DeafInterrupt => Some("deaf_interrupt"),
        }
    }

    pub(crate) fn from_env() -> Self {
        match std::env::var(Self::ENV).as_deref() {
            Ok("exit_mid_request") => Self::ExitMidRequest,
            Ok("async_abort") => Self::AsyncAbort,
            Ok("deaf_interrupt") => Self::DeafInterrupt,
            _ => Self::None,
        }
    }
}

/// Fail-closed limit validation shared by the host and the worker process.
pub(crate) fn validate_limits(
    memory: u64,
    stack: u64,
) -> Result<(usize, usize), QuickJsConfigError> {
    let memory_bytes =
        usize::try_from(memory).map_err(|_| QuickJsConfigError::MemoryLimitUnrepresentable)?;
    let stack_bytes =
        usize::try_from(stack).map_err(|_| QuickJsConfigError::StackAboveWrapperLimit)?;
    if stack_bytes > WRAPPER_STACK_LIMIT {
        return Err(QuickJsConfigError::StackAboveWrapperLimit);
    }
    if stack_bytes < MIN_STACK {
        return Err(QuickJsConfigError::StackBelowMinimum);
    }
    if memory_bytes < MIN_MEMORY {
        return Err(QuickJsConfigError::MemoryBelowMinimum);
    }
    Ok((memory_bytes, stack_bytes))
}

/// Realm limits: the worker uses them for `verify_limits`; with `test-faults` the same type is also
/// the in-process engine (realm on a sized thread inside the caller's process; hostile source can
/// abort that process, see module docs), reachable only through `QuickJsOptions::unsafe_in_process`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QuickJsEngine {
    memory_bytes: usize,
    stack_bytes: usize,
}

impl QuickJsEngine {
    #[cfg(feature = "test-faults")]
    pub(crate) fn new(quotas: &Quotas) -> Result<Self, QuickJsConfigError> {
        let (memory_bytes, stack_bytes) =
            validate_limits(quotas.memory_bytes(), quotas.stack_bytes())?;
        Ok(Self {
            memory_bytes,
            stack_bytes,
        })
    }

    pub(crate) fn from_raw(memory: u64, stack: u64) -> Result<Self, QuickJsConfigError> {
        let (memory_bytes, stack_bytes) = validate_limits(memory, stack)?;
        Ok(Self {
            memory_bytes,
            stack_bytes,
        })
    }

    /// Applies the configured limits to a throw-away runtime and reads the heap limit back.
    pub(crate) fn verify_limits(&self) -> Result<LimitReadback, EngineFailure> {
        let runtime = Runtime::new().map_err(|_| EngineFailure::Internal)?;
        apply_limits(&runtime, self.memory_bytes, self.stack_bytes)
    }
}

fn apply_limits(
    runtime: &Runtime,
    memory_bytes: usize,
    stack_bytes: usize,
) -> Result<LimitReadback, EngineFailure> {
    runtime.set_memory_limit(memory_bytes);
    let readback = runtime.memory_usage().malloc_limit;
    if i64::try_from(memory_bytes).ok() != Some(readback) {
        return Err(EngineFailure::Internal);
    }
    runtime.set_max_stack_size(stack_bytes);
    Ok(LimitReadback {
        memory_limit: readback,
        stack_applied: stack_bytes,
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ReadKey {
    pub(crate) path: String,
    pub(crate) tick: Option<i64>,
}

pub(crate) enum Pending {
    Read(ReadKey),
    Utc,
    Failure(EngineFailure),
}

pub(crate) enum RunEnd {
    Value(JsValue),
    Failure(EngineFailure),
    Pending(Pending),
}

pub(crate) struct RunReport {
    pub(crate) polls: u64,
    pub(crate) end: RunEnd,
    /// Runtime and context were released by the thread that created them.
    pub(crate) dropped: bool,
}

pub(crate) struct Input<'a> {
    pub(crate) source: &'a str,
    pub(crate) max_nodes: usize,
    pub(crate) memory_bytes: usize,
    pub(crate) stack_bytes: usize,
    pub(crate) allowance: u64,
    pub(crate) cancel: CancellationToken,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) deadline_ns: u64,
    pub(crate) cache: &'a HashMap<ReadKey, Vec<f64>>,
    pub(crate) utc: Option<i64>,
    #[cfg(feature = "test-faults")]
    pub(crate) fault: WorkerTestFault,
}

impl Input<'_> {
    /// Test fault: the realm installs no interrupt handler (`test-faults` only).
    #[cfg(feature = "test-faults")]
    fn deaf_interrupt(&self) -> bool {
        self.fault == WorkerTestFault::DeafInterrupt
    }

    #[cfg(not(feature = "test-faults"))]
    fn deaf_interrupt(&self) -> bool {
        false
    }
}

/// Why our own interrupt handler stopped the script: an engine signal, never script text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tripped {
    Fuel,
    Cancel,
    Deadline,
}

struct Shared {
    polls: Cell<u64>,
    armed: Cell<bool>,
    stop: Cell<bool>,
    tripped: Cell<Option<Tripped>>,
    pending: RefCell<Option<Pending>>,
}

impl Shared {
    fn record(&self, pending: Pending) {
        let mut slot = self.pending.borrow_mut();
        if slot.is_none() {
            *slot = Some(pending);
        }
        self.stop.set(true);
    }

    fn trip(&self, cause: Tripped) -> bool {
        if self.tripped.get().is_none() {
            self.tripped.set(Some(cause));
        }
        true
    }
}

pub(crate) fn internal_report() -> RunReport {
    RunReport {
        polls: 0,
        end: RunEnd::Failure(EngineFailure::Internal),
        dropped: false,
    }
}

/// Test-fault body: the unmitigated slice-2 context. QuickJS aborts the process when the runtime
/// is freed with the leaked async function object still alive.
#[cfg(feature = "test-faults")]
fn reproduce_unmitigated_async(input: &Input<'_>) -> RunReport {
    let Ok(runtime) = Runtime::new() else {
        return internal_report();
    };
    if apply_limits(&runtime, input.memory_bytes, input.stack_bytes).is_err() {
        return internal_report();
    }
    let Ok(context) = Context::builder().with::<intrinsic::Eval>().build(&runtime) else {
        return internal_report();
    };
    context.with(|ctx| {
        let _ = ctx.eval::<Value<'_>, _>(input.source);
    });
    drop(context);
    drop(runtime);
    RunReport {
        polls: 0,
        end: RunEnd::Failure(EngineFailure::Internal),
        dropped: true,
    }
}

/// One realm: fresh Runtime + context, one script run, everything released before returning.
pub(crate) fn run_realm(input: Input<'_>) -> RunReport {
    #[cfg(feature = "test-faults")]
    {
        if input.fault == WorkerTestFault::AsyncAbort && input.source.contains("async") {
            return reproduce_unmitigated_async(&input);
        }
    }
    let shared = Rc::new(Shared {
        polls: Cell::new(0),
        armed: Cell::new(false),
        stop: Cell::new(false),
        tripped: Cell::new(None),
        pending: RefCell::new(None),
    });
    let Ok(runtime) = Runtime::new() else {
        return internal_report();
    };
    if apply_limits(&runtime, input.memory_bytes, input.stack_bytes).is_err() {
        return internal_report();
    }
    if !input.deaf_interrupt() {
        let shared = Rc::clone(&shared);
        let (cancel, clock) = (input.cancel.clone(), Arc::clone(&input.clock));
        let (allowance, deadline_ns) = (input.allowance, input.deadline_ns);
        runtime.set_interrupt_handler(Some(Box::new(move || {
            if !shared.armed.get() {
                return false;
            }
            let polls = shared.polls.get().saturating_add(1);
            shared.polls.set(polls);
            if polls > allowance {
                return shared.trip(Tripped::Fuel);
            }
            if shared.stop.get() {
                return true;
            }
            if cancel.check().is_err() {
                return shared.trip(Tripped::Cancel);
            }
            if clock.now_ns() > deadline_ns {
                return shared.trip(Tripped::Deadline);
            }
            false
        })));
    }
    let Ok(context) = Context::builder()
        .with::<intrinsic::Eval>()
        .with::<intrinsic::Promise>()
        .build(&runtime)
    else {
        return internal_report();
    };
    let mut end = context.with(|ctx| session(ctx, &input, &shared));
    if let Some(pending) = shared.pending.borrow_mut().take() {
        end = RunEnd::Pending(pending);
    }
    let polls = shared.polls.get();
    drop(context);
    drop(runtime);
    RunReport {
        polls,
        end,
        dropped: true,
    }
}

fn invalid_argument() -> EngineFailure {
    EngineFailure::Host(ExprError::new(ExprErrorCode::ReferenceMissing))
}

fn install_host(
    ctx: &Ctx<'_>,
    shared: &Rc<Shared>,
    cache: Rc<HashMap<ReadKey, Vec<f64>>>,
    utc: Option<i64>,
) -> rquickjs::Result<()> {
    let read_shared = Rc::clone(shared);
    let read = Function::new(
        ctx.clone(),
        move |path: String, tick: Opt<f64>| -> Vec<f64> {
            if read_shared.stop.get() {
                return Vec::new();
            }
            let tick = match tick.0 {
                None => None,
                Some(t) if t.is_finite() && t.fract() == 0.0 && t.abs() <= 9.0e15 => Some(t as i64),
                Some(_) => {
                    read_shared.record(Pending::Failure(invalid_argument()));
                    return Vec::new();
                }
            };
            let key = ReadKey { path, tick };
            match cache.get(&key) {
                Some(values) => values.clone(),
                None => {
                    read_shared.record(Pending::Read(key));
                    Vec::new()
                }
            }
        },
    )?;
    let utc_shared = Rc::clone(shared);
    let utc_fn = Function::new(ctx.clone(), move || -> f64 {
        match utc {
            Some(value) => value as f64,
            None => {
                utc_shared.record(Pending::Utc);
                0.0
            }
        }
    })?;
    let globals = ctx.globals();
    globals.set("hostRead", read)?;
    globals.set("hostUtcMs", utc_fn)?;
    Ok(())
}

/// The two trusted bootstrap functions.
struct Tools<'js> {
    extract: Function<'js>,
    describe: Function<'js>,
}

fn load_tools<'js>(ctx: &Ctx<'js>) -> Option<Tools<'js>> {
    let array: Array<'js> = ctx.eval(BOOTSTRAP).ok()?;
    Some(Tools {
        extract: array.get(0).ok()?,
        describe: array.get(1).ok()?,
    })
}

fn session<'js>(ctx: Ctx<'js>, input: &Input<'_>, shared: &Rc<Shared>) -> RunEnd {
    let cache = Rc::new(input.cache.clone());
    if install_host(&ctx, shared, cache, input.utc).is_err() {
        return RunEnd::Failure(EngineFailure::Internal);
    }
    let Some(tools) = load_tools(&ctx) else {
        return RunEnd::Failure(EngineFailure::Internal);
    };
    let mut options = EvalOptions::default();
    options.strict = true;
    options.filename = Some(FILENAME.to_owned());
    // Armed from the first user instruction until the last value has left the realm: the user
    // source, the result extractor and the error describer all run under the interrupt.
    shared.armed.set(true);
    let end = evaluate(&ctx, input, shared, &tools, options);
    shared.armed.set(false);
    end
}

fn evaluate<'js>(
    ctx: &Ctx<'js>,
    input: &Input<'_>,
    shared: &Rc<Shared>,
    tools: &Tools<'js>,
    options: EvalOptions,
) -> RunEnd {
    let result: rquickjs::Result<Value<'js>> = ctx.eval_with_options(input.source, options);
    if shared.stop.get() {
        // A host miss/failure was recorded: everything after it is unfaithful and discarded.
        return RunEnd::Failure(EngineFailure::Internal);
    }
    match result {
        Err(error) => RunEnd::Failure(classify(ctx, shared, tools, error)),
        Ok(value) => match tools
            .extract
            .call::<_, Value<'js>>((value, input.max_nodes))
        {
            Ok(extracted) => convert(extracted),
            Err(error) => RunEnd::Failure(classify(ctx, shared, tools, error)),
        },
    }
}

fn convert(value: Value<'_>) -> RunEnd {
    if let Some(number) = value.as_number() {
        return RunEnd::Value(JsValue::Number(number));
    }
    let Some(array) = value.into_array() else {
        return RunEnd::Failure(EngineFailure::Output);
    };
    let mut numbers = Vec::with_capacity(array.len());
    for index in 0..array.len() {
        match array.get::<f64>(index) {
            Ok(number) => numbers.push(JsValue::Number(number)),
            Err(_) => return RunEnd::Failure(EngineFailure::Output),
        }
    }
    RunEnd::Value(JsValue::Array(numbers))
}

fn span_of(stack: &str) -> Option<SourceSpan> {
    let marker = format!("{FILENAME}:");
    let rest = stack.get(stack.find(&marker)?.checked_add(marker.len())?..)?;
    let digits: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ':')
        .collect();
    let mut parts = digits.split(':');
    let line = parts.next()?.parse().ok()?;
    let column = parts.next()?.parse().ok()?;
    Some(SourceSpan { line, column })
}

/// Budget classes from host-side state only (our interrupt handler, the rquickjs error type), then
/// the trusted describer for the kind of a thrown value (Parse / ReferenceMissing / other). No text a
/// script can write selects a class, and no property of a thrown value is read through
/// user-reachable code. A catchable QuickJS stack- or heap-limit error is indistinguishable from a
/// user throw (diagnostics/motion-js-classify.md) and is therefore `Thrown { Other }`.
fn classify<'js>(
    ctx: &Ctx<'js>,
    shared: &Rc<Shared>,
    tools: &Tools<'js>,
    error: rquickjs::Error,
) -> EngineFailure {
    let thrown = matches!(error, rquickjs::Error::Exception).then(|| ctx.catch());
    match shared.tripped.get() {
        Some(Tripped::Fuel) => return EngineFailure::Budget(BudgetKind::Fuel),
        Some(Tripped::Cancel) => return EngineFailure::Canceled,
        Some(Tripped::Deadline) => return EngineFailure::Budget(BudgetKind::Deadline),
        None => {}
    }
    let Some(thrown) = thrown else {
        return match error {
            rquickjs::Error::Allocation => EngineFailure::Budget(BudgetKind::Memory),
            _ => EngineFailure::Internal,
        };
    };
    let other = |span| EngineFailure::Thrown {
        kind: ThrownKind::Other,
        span,
    };
    let Ok(described) = tools.describe.call::<_, Array<'js>>((thrown,)) else {
        return other(None);
    };
    let kind = described.get::<u32>(0).unwrap_or(0);
    let span = described
        .get::<String>(1)
        .ok()
        .and_then(|stack| span_of(&stack));
    match kind {
        1 => EngineFailure::Parse(span),
        2 => EngineFailure::Thrown {
            kind: ThrownKind::Reference,
            span,
        },
        _ => other(span),
    }
}

/// What one realm run produced, or why no report exists.
pub(crate) enum RunOutcome {
    /// `setup_ns` is host-side setup time (worker process start) measured on the job clock; the
    /// replay loop grants it back to the evaluation deadline because it is not script time.
    Report { report: RunReport, setup_ns: u64 },
    /// The run produced no report. `dropped` is true only when the realm is provably gone (the
    /// owning process was reaped); false makes the provider poison itself.
    Failed {
        failure: EngineFailure,
        dropped: bool,
    },
}

/// Replay loop shared by the in-process and the worker backend: identical limits check, miss/replay
/// host reads, fuel charging and result handling, so both backends yield identical results.
pub(crate) fn drive(
    memory_bytes: usize,
    stack_bytes: usize,
    job: &EvalJob<'_>,
    host: &mut dyn HostBridge,
    realm: &mut RealmReceipt,
    mut run_once: impl FnMut(Input<'_>) -> RunOutcome,
) -> Result<JsValue, EngineFailure> {
    let limits_match = u64::try_from(memory_bytes) == Ok(job.limits.memory_bytes)
        && u64::try_from(stack_bytes) == Ok(job.limits.stack_bytes);
    if !limits_match {
        return Err(EngineFailure::Internal);
    }
    let mut cache: HashMap<ReadKey, Vec<f64>> = HashMap::new();
    let mut utc: Option<i64> = None;
    let mut granted_ns = 0_u64;
    for _ in 0..MAX_REPLAYS {
        host.checkpoint()?;
        let allowance = host.fuel_remaining();
        let input = Input {
            source: job.source,
            max_nodes: job.limits.max_output_nodes,
            memory_bytes,
            stack_bytes,
            allowance,
            cancel: job.cancel.clone(),
            clock: Arc::clone(&job.clock),
            deadline_ns: job.deadline_ns.saturating_add(granted_ns),
            cache: &cache,
            utc,
            #[cfg(feature = "test-faults")]
            fault: WorkerTestFault::None,
        };
        let report = match run_once(input) {
            RunOutcome::Report { report, setup_ns } => {
                granted_ns = granted_ns.saturating_add(setup_ns);
                host.grant_time(setup_ns);
                report
            }
            RunOutcome::Failed { failure, dropped } => {
                realm.dropped &= dropped;
                return Err(failure);
            }
        };
        realm.dropped &= report.dropped;
        // Cancel, deadline and fuel exhaustion all surface here, ahead of the script outcome.
        host.charge(report.polls)?;
        match report.end {
            RunEnd::Value(value) => return Ok(value),
            RunEnd::Failure(failure) => return Err(failure),
            RunEnd::Pending(Pending::Failure(failure)) => return Err(failure),
            RunEnd::Pending(Pending::Read(key)) => {
                let values = host.read(&key.path, key.tick.map(EvalTick::new))?;
                cache.insert(key, values);
            }
            RunEnd::Pending(Pending::Utc) => utc = Some(host.utc_ms()?),
        }
    }
    Err(EngineFailure::Budget(BudgetKind::HostReads))
}

#[cfg(feature = "test-faults")]
impl QuickJsEngine {
    fn run_in_process(&self, input: Input<'_>) -> RunOutcome {
        let stack = self.stack_bytes.saturating_add(THREAD_STACK_HEADROOM);
        let report = thread::scope(|scope| {
            let spawned = thread::Builder::new()
                .name("motion-js-quickjs".to_owned())
                .stack_size(stack)
                .spawn_scoped(scope, move || run_realm(input));
            spawned.ok()?.join().ok()
        });
        match report {
            Some(report) => RunOutcome::Report {
                report,
                setup_ns: 0,
            },
            None => RunOutcome::Failed {
                failure: EngineFailure::Internal,
                dropped: false,
            },
        }
    }
}

#[cfg(feature = "test-faults")]
impl Engine for QuickJsEngine {
    fn identity(&self) -> EngineIdentity {
        EngineIdentity {
            name: "quickjs-ng via rquickjs (in-process, unsafe)",
            version: ENGINE_VERSION,
            native: true,
        }
    }

    fn run(&self, job: &EvalJob<'_>, host: &mut dyn HostBridge) -> EngineRun {
        let mut realm = RealmReceipt {
            fresh: true,
            dropped: true,
        };
        let outcome = drive(
            self.memory_bytes,
            self.stack_bytes,
            job,
            host,
            &mut realm,
            |input| self.run_in_process(input),
        );
        EngineRun { outcome, realm }
    }
}

/// How the QuickJS backend is hosted: always worker-process isolation in product builds.
#[derive(Clone, Debug, Default)]
pub struct QuickJsOptions {
    /// TEST-ONLY escape hatch (`test-faults` feature): run QuickJS inside the caller's process.
    /// Hostile source can abort that process and no wall-clock deadline can stop a non-cooperative
    /// script.
    #[cfg(feature = "test-faults")]
    pub unsafe_in_process: bool,
    /// Worker-process settings.
    pub worker: WorkerConfig,
}

impl QuickJsOptions {
    /// Explicit opt-in to the in-process engine (`test-faults` feature, tests only).
    #[cfg(feature = "test-faults")]
    pub fn unsafe_in_process() -> Self {
        Self {
            unsafe_in_process: true,
            ..Self::default()
        }
    }

    /// Worker isolation with an explicit worker executable.
    pub fn with_worker_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.worker.program = Some(program.into());
        self
    }
}

enum Backend {
    #[cfg(feature = "test-faults")]
    InProcess(QuickJsEngine),
    Worker(Box<WorkerEngine>),
}

/// The QuickJS `Engine` adapter handed to `MotionJsProvider`: worker-process isolated (with
/// `test-faults`, in-process when `QuickJsOptions::unsafe_in_process` was requested).
pub struct QuickJsBackend(Backend);

impl QuickJsBackend {
    /// Fail-closed configuration from validated `Quotas`.
    pub fn new(quotas: &Quotas, options: QuickJsOptions) -> Result<Self, QuickJsConfigError> {
        #[cfg(feature = "test-faults")]
        {
            if options.unsafe_in_process {
                return Ok(Self(Backend::InProcess(QuickJsEngine::new(quotas)?)));
            }
        }
        Ok(Self(Backend::Worker(Box::new(WorkerEngine::new(
            quotas,
            &options.worker,
        )?))))
    }

    pub fn is_worker(&self) -> bool {
        matches!(self.0, Backend::Worker(_))
    }

    /// Process counters of the worker pool (`None` for the in-process engine).
    pub fn worker_stats(&self) -> Option<WorkerStats> {
        match &self.0 {
            #[cfg(feature = "test-faults")]
            Backend::InProcess(_) => None,
            Backend::Worker(worker) => Some(worker.stats()),
        }
    }

    /// Applies the configured limits to a throw-away runtime inside the worker process and reads
    /// the heap limit back.
    pub fn verify_limits(&self) -> Result<LimitReadback, EngineFailure> {
        match &self.0 {
            #[cfg(feature = "test-faults")]
            Backend::InProcess(engine) => engine.verify_limits(),
            Backend::Worker(worker) => worker.verify_limits(),
        }
    }
}

impl std::fmt::Debug for QuickJsBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuickJsBackend")
            .field("worker", &self.is_worker())
            .finish()
    }
}

impl Engine for QuickJsBackend {
    fn identity(&self) -> EngineIdentity {
        match &self.0 {
            #[cfg(feature = "test-faults")]
            Backend::InProcess(engine) => engine.identity(),
            Backend::Worker(worker) => worker.identity(),
        }
    }

    fn run(&self, job: &EvalJob<'_>, host: &mut dyn HostBridge) -> EngineRun {
        match &self.0 {
            #[cfg(feature = "test-faults")]
            Backend::InProcess(engine) => engine.run(job, host),
            Backend::Worker(worker) => worker.run(job, host),
        }
    }
}
