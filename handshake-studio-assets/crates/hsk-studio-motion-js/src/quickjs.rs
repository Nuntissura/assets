//! QuickJS backend (cargo feature `quickjs`, default OFF): `rquickjs =0.14.0` over the bundled
//! QuickJS-NG 0.16.2 C engine. Implements the neutral `Engine` adapter; no rquickjs type appears in
//! any public signature of this crate.
//!
//! Controls enforced in-process (adoption review EXPR-PROVIDER-xx; NOT worker isolation):
//! * 02: explicit nonzero heap limit applied BEFORE any context/parse/eval, accepted value read back
//!   (`malloc_limit`) and a mismatch fails closed; stack limit applied before eval and rejected at
//!   configuration when the wrapper would map it to 0/unlimited (above 16 MiB). The wrapper offers no
//!   stack readback; the effect is proven by a runaway-recursion test, not by a getter.
//! * 03: no `allocator`/`rust-alloc` feature; the default C allocator accounting is what the limit meters.
//! * 04: interrupt = cooperative polling. One engine poll (about 10,000 interpreter branch/call
//!   steps) costs one unit of the SHARED Motion `Fuel`; cancel and the monotonic deadline are polled
//!   there too. This is a bounded-cooperation budget, NOT semantic AST fuel (EXPR-PROVIDER-13 stays
//!   NOT_PROVEN) and NOT a hard wall-clock guarantee for parsing or long C builtins.
//! * 05 (partial): `eval` and the four function constructors (Function, GeneratorFunction,
//!   AsyncFunction, AsyncGeneratorFunction; the only `js_function_constructor` registrations in the
//!   bundled source) are replaced by non-configurable throwers before user source runs, and the
//!   direct-eval identity check (`ctx->eval_obj`) can no longer match. The context still carries the
//!   Eval intrinsic because compiling ANY source needs it: a separate trusted compile realm is NOT
//!   built (no safe wrapper API), so completeness is NOT_PROVEN.
//! * 06 (partial): contexts carry only base objects + Eval: no Date, Proxy, Promise, JSON, RegExp,
//!   typed arrays, loader, console or libc APIs; `Math.random` throws; user source is strict mode.
//! * 07: a fresh Runtime and context per run, dropped on the worker thread that created them.
//! * 10: results leave the realm only through a trusted extractor that uses captured intrinsics and
//!   own-data-property descriptors, so getters, proxies and user coercion never run.
//!
//! Host reads use deterministic miss/replay: the script runs in a fresh realm against an
//! evaluation-local cache; the first uncached `hostRead` records a miss, the run is discarded, the
//! host performs that single read on the caller thread (shared Fuel, quotas), and the script is
//! replayed. Every distinct (path, tick) is charged once per evaluation.
use crate::{
    clock::Clock,
    engine::{
        BudgetKind, Engine, EngineFailure, EngineIdentity, EngineRun, EvalJob, HostBridge, JsValue,
        RealmReceipt, ThrownKind,
    },
    limits::Quotas,
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_motion::{EvalTick, ExprError, ExprErrorCode, SourceSpan};
use rquickjs::{
    CaughtError, Context, Ctx, Exception, Function, Runtime, Value,
    context::{EvalOptions, intrinsic},
    function::Opt,
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::Arc,
    thread,
};

/// rquickjs maps any stack limit above this to 0 (= unlimited); configuration must reject it.
pub const WRAPPER_STACK_LIMIT: usize = 1 << 24;
const MIN_STACK: usize = 64 << 10;
const MIN_MEMORY: usize = 1 << 20;
const THREAD_STACK_HEADROOM: usize = 1 << 20;
const MAX_REPLAYS: usize = 4096;
const FILENAME: &str = "expr";
const UNAVAILABLE_MARKER: &str = "motionjs-unavailable:";

/// Trusted bootstrap: closes the dynamic-compile entry points, removes ambient randomness and
/// returns the result extractor (kept by Rust, unreachable from user code).
const BOOTSTRAP: &str = r#"(function (g) {
  "use strict";
  var def = Object.defineProperty;
  var getProto = Object.getPrototypeOf;
  var gopd = Object.getOwnPropertyDescriptor;
  var isArray = Array.isArray;
  var TypeErr = TypeError;
  function deny(obj, key, what) {
    var thrower = function () { throw new TypeErr("motionjs-unavailable: " + what); };
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
  deny(Math, "random", "Math.random");
  function extract(v, max) {
    if (typeof v === "number") { return v; }
    if (!isArray(v)) { return undefined; }
    var n = v.length;
    if (typeof n !== "number" || n > max) { return undefined; }
    var out = [];
    for (var k = 0; k < n; k++) {
      var d = gopd(v, k);
      if (d === undefined || gopd(d, "value") === undefined) { return undefined; }
      if (typeof d.value !== "number") { return undefined; }
      def(out, k, { value: d.value, writable: true, enumerable: true, configurable: true });
    }
    return out;
  }
  return extract;
})(globalThis)"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickJsConfigError {
    /// The wrapper would silently treat this stack size as unlimited.
    StackAboveWrapperLimit,
    StackBelowMinimum,
    MemoryBelowMinimum,
    MemoryLimitUnrepresentable,
}

impl QuickJsConfigError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::StackAboveWrapperLimit => "stack_above_wrapper_limit",
            Self::StackBelowMinimum => "stack_below_minimum",
            Self::MemoryBelowMinimum => "memory_below_minimum",
            Self::MemoryLimitUnrepresentable => "memory_limit_unrepresentable",
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuickJsEngine {
    memory_bytes: usize,
    stack_bytes: usize,
}

impl QuickJsEngine {
    /// Fail-closed configuration from validated `Quotas`.
    pub fn new(quotas: &Quotas) -> Result<Self, QuickJsConfigError> {
        let memory_bytes = usize::try_from(quotas.memory_bytes())
            .map_err(|_| QuickJsConfigError::MemoryLimitUnrepresentable)?;
        let stack_bytes = usize::try_from(quotas.stack_bytes())
            .map_err(|_| QuickJsConfigError::StackAboveWrapperLimit)?;
        if stack_bytes > WRAPPER_STACK_LIMIT {
            return Err(QuickJsConfigError::StackAboveWrapperLimit);
        }
        if stack_bytes < MIN_STACK {
            return Err(QuickJsConfigError::StackBelowMinimum);
        }
        if memory_bytes < MIN_MEMORY {
            return Err(QuickJsConfigError::MemoryBelowMinimum);
        }
        Ok(Self { memory_bytes, stack_bytes })
    }

    /// Applies the configured limits to a throw-away runtime and reads the heap limit back.
    pub fn verify_limits(&self) -> Result<LimitReadback, EngineFailure> {
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
    Ok(LimitReadback { memory_limit: readback, stack_applied: stack_bytes })
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ReadKey {
    path: String,
    tick: Option<i64>,
}

enum Pending {
    Read(ReadKey),
    Utc,
    Failure(EngineFailure),
}

enum RunEnd {
    Value(JsValue),
    Failure(EngineFailure),
    Pending(Pending),
}

struct RunReport {
    polls: u64,
    end: RunEnd,
    /// Runtime and context were released on the worker thread.
    dropped: bool,
}

struct Input<'a> {
    source: &'a str,
    max_nodes: usize,
    memory_bytes: usize,
    stack_bytes: usize,
    allowance: u64,
    cancel: CancellationToken,
    clock: Arc<dyn Clock>,
    deadline_ns: u64,
    cache: &'a HashMap<ReadKey, Vec<f64>>,
    utc: Option<i64>,
}

struct Shared {
    polls: Cell<u64>,
    armed: Cell<bool>,
    stop: Cell<bool>,
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
}

fn internal_report() -> RunReport {
    RunReport { polls: 0, end: RunEnd::Failure(EngineFailure::Internal), dropped: false }
}

fn run_worker(input: Input<'_>) -> RunReport {
    let shared = Rc::new(Shared {
        polls: Cell::new(0),
        armed: Cell::new(false),
        stop: Cell::new(false),
        pending: RefCell::new(None),
    });
    let Ok(runtime) = Runtime::new() else {
        return internal_report();
    };
    if apply_limits(&runtime, input.memory_bytes, input.stack_bytes).is_err() {
        return internal_report();
    }
    {
        let shared = Rc::clone(&shared);
        let (cancel, clock) = (input.cancel.clone(), Arc::clone(&input.clock));
        let (allowance, deadline_ns) = (input.allowance, input.deadline_ns);
        runtime.set_interrupt_handler(Some(Box::new(move || {
            if !shared.armed.get() {
                return false;
            }
            let polls = shared.polls.get().saturating_add(1);
            shared.polls.set(polls);
            polls > allowance
                || shared.stop.get()
                || cancel.check().is_err()
                || clock.now_ns() > deadline_ns
        })));
    }
    let Ok(context) = Context::builder().with::<intrinsic::Eval>().build(&runtime) else {
        return internal_report();
    };
    let mut end = context.with(|ctx| session(ctx, &input, &shared));
    if let Some(pending) = shared.pending.borrow_mut().take() {
        end = RunEnd::Pending(pending);
    }
    let polls = shared.polls.get();
    drop(context);
    drop(runtime);
    RunReport { polls, end, dropped: true }
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
    let read = Function::new(ctx.clone(), move |path: String, tick: Opt<f64>| -> Vec<f64> {
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
    })?;
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

fn session<'js>(ctx: Ctx<'js>, input: &Input<'_>, shared: &Rc<Shared>) -> RunEnd {
    let cache = Rc::new(input.cache.clone());
    if install_host(&ctx, shared, cache, input.utc).is_err() {
        return RunEnd::Failure(EngineFailure::Internal);
    }
    let extract: Function<'js> = match ctx.eval(BOOTSTRAP) {
        Ok(function) => function,
        Err(_) => return RunEnd::Failure(EngineFailure::Internal),
    };
    let mut options = EvalOptions::default();
    options.strict = true;
    options.filename = Some(FILENAME.to_owned());
    shared.armed.set(true);
    let result: rquickjs::Result<Value<'js>> = ctx.eval_with_options(input.source, options);
    shared.armed.set(false);
    if shared.stop.get() {
        // A host miss/failure was recorded: everything after it is unfaithful and discarded.
        return RunEnd::Failure(EngineFailure::Internal);
    }
    match result {
        Err(error) => RunEnd::Failure(classify(&ctx, error)),
        Ok(value) => match extract.call::<_, Value<'js>>((value, input.max_nodes)) {
            Ok(extracted) => convert(extracted),
            Err(error) => RunEnd::Failure(classify(&ctx, error)),
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

fn span_of(exception: &Exception<'_>) -> Option<SourceSpan> {
    let stack = exception.stack()?;
    let marker = format!("{FILENAME}:");
    let rest = &stack[stack.find(&marker)? + marker.len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == ':').collect();
    let mut parts = digits.split(':');
    let line = parts.next()?.parse().ok()?;
    let column = parts.next()?.parse().ok()?;
    Some(SourceSpan { line, column })
}

fn classify<'js>(ctx: &Ctx<'js>, error: rquickjs::Error) -> EngineFailure {
    match CaughtError::from_error(ctx, error) {
        CaughtError::Exception(exception) => {
            let name = exception
                .as_object()
                .get::<_, Option<String>>("name")
                .ok()
                .flatten()
                .unwrap_or_default();
            let message = exception.message().unwrap_or_default();
            let span = span_of(&exception);
            match name.as_str() {
                "SyntaxError" => EngineFailure::Parse(span),
                "ReferenceError" => EngineFailure::Thrown { kind: ThrownKind::Reference, span },
                "RangeError" if message.contains("Maximum call stack") => {
                    EngineFailure::Budget(BudgetKind::Stack)
                }
                "InternalError" if message.contains("out of memory") => {
                    EngineFailure::Budget(BudgetKind::Memory)
                }
                "InternalError" => EngineFailure::Internal,
                "TypeError" if message.starts_with(UNAVAILABLE_MARKER) => {
                    EngineFailure::Unsupported(span)
                }
                _ => EngineFailure::Thrown { kind: ThrownKind::Other, span },
            }
        }
        CaughtError::Value(_) => EngineFailure::Thrown { kind: ThrownKind::Other, span: None },
        CaughtError::Error(rquickjs::Error::Allocation) => EngineFailure::Budget(BudgetKind::Memory),
        CaughtError::Error(_) => EngineFailure::Internal,
    }
}

impl QuickJsEngine {
    fn run_once(&self, input: Input<'_>) -> Option<RunReport> {
        let stack = self.stack_bytes + THREAD_STACK_HEADROOM;
        thread::scope(|scope| {
            let spawned = thread::Builder::new()
                .name("motion-js-quickjs".to_owned())
                .stack_size(stack)
                .spawn_scoped(scope, move || run_worker(input));
            spawned.ok()?.join().ok()
        })
    }

    fn drive(
        &self,
        job: &EvalJob<'_>,
        host: &mut dyn HostBridge,
        realm: &mut RealmReceipt,
    ) -> Result<JsValue, EngineFailure> {
        let limits_match = u64::try_from(self.memory_bytes) == Ok(job.limits.memory_bytes)
            && u64::try_from(self.stack_bytes) == Ok(job.limits.stack_bytes);
        if !limits_match {
            return Err(EngineFailure::Internal);
        }
        let mut cache: HashMap<ReadKey, Vec<f64>> = HashMap::new();
        let mut utc: Option<i64> = None;
        for _ in 0..MAX_REPLAYS {
            host.checkpoint()?;
            let allowance = host.fuel_remaining();
            let input = Input {
                source: job.source,
                max_nodes: job.limits.max_output_nodes,
                memory_bytes: self.memory_bytes,
                stack_bytes: self.stack_bytes,
                allowance,
                cancel: job.cancel.clone(),
                clock: Arc::clone(&job.clock),
                deadline_ns: job.deadline_ns,
                cache: &cache,
                utc,
            };
            let Some(report) = self.run_once(input) else {
                realm.dropped = false;
                return Err(EngineFailure::Internal);
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
}

impl Engine for QuickJsEngine {
    fn identity(&self) -> EngineIdentity {
        EngineIdentity {
            name: "quickjs-ng via rquickjs",
            version: "quickjs-ng 0.16.2, rquickjs 0.14.0",
            native: true,
        }
    }

    fn run(&self, job: &EvalJob<'_>, host: &mut dyn HostBridge) -> EngineRun {
        let mut realm = RealmReceipt { fresh: true, dropped: true };
        let outcome = self.drive(job, host, &mut realm);
        EngineRun { outcome, realm }
    }
}
