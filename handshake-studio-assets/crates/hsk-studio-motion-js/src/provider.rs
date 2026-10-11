//! `MotionJsProvider<E>`: implements Motion's `ExpressionProvider` port over an `Engine` adapter.
//! Owns quota/fuel/deadline accounting, typed error mapping, fresh-realm and permit accounting.
//! Pure Motion never depends on this crate; a thrown script error never escapes as a panic.
use crate::{
    clock::{Clock, SystemClock},
    engine::{
        BudgetKind, Engine, EngineFailure, EngineFault, EngineIdentity, EngineLimits, EvalJob,
        HostBridge, JsValue, SourceLanguage, ThrownKind,
    },
    limits::Quotas,
    profile::{ProfileValidator, ProfileViolation, ViolationClass},
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_motion::{
    DateProfile, EvalTick, ExprError, ExprErrorCode, ExprRequest, ExpressionProvider, Fuel,
    HostRead, PropertyPath,
};
use hsk_studio_observe::{Observation, Observe, Outcome, Receipt, SinkPort};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    thread::{self, ThreadId},
};

/// Provider/profile identifier recorded in Motion receipts.
pub const PROFILE_ID: &str = "motion-js-es2018@1";

#[derive(Debug, Default)]
struct Counters {
    evaluations: AtomicU64,
    realms_started: AtomicU64,
    realms_retired: AtomicU64,
    charged_units: AtomicU64,
    checkpoints: AtomicU64,
    host_reads: AtomicU64,
    validation_rejected: AtomicU64,
    denials: [AtomicU64; BudgetKind::COUNT],
    faults: [AtomicU64; EngineFault::COUNT],
}

/// Monotonic counters since construction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProviderStats {
    pub evaluations: u64,
    pub realms_started: u64,
    pub realms_retired: u64,
    pub charged_units: u64,
    pub checkpoints: u64,
    pub host_reads: u64,
    /// Sources refused by the profile validator before any engine ran.
    pub validation_rejected: u64,
    /// Budget denials indexed by `BudgetKind::index()`.
    pub denials: [u64; BudgetKind::COUNT],
    /// Out-of-process isolation faults indexed by `EngineFault::index()`.
    pub faults: [u64; EngineFault::COUNT],
}

impl ProviderStats {
    pub fn denied(&self, kind: BudgetKind) -> u64 {
        self.denials[kind.index()]
    }

    pub fn faulted(&self, kind: EngineFault) -> u64 {
        self.faults[kind.index()]
    }
}

/// Result of consuming the single owner (`retire`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetireReport {
    pub realms_started: u64,
    pub realms_retired: u64,
    /// No evaluation permit is still held.
    pub permits_released: bool,
    pub poisoned: bool,
    /// `retire` ran on the thread that built the provider (native runtimes are thread-affine).
    pub owner_thread: bool,
}

impl RetireReport {
    pub fn is_clean(&self) -> bool {
        self.realms_started == self.realms_retired
            && self.permits_released
            && !self.poisoned
            && self.owner_thread
    }
}

struct Permit<'a>(&'a AtomicU32);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct MotionJsProvider<E: Engine> {
    engine: E,
    quotas: Quotas,
    date: DateProfile,
    clock: Arc<dyn Clock>,
    owner: ThreadId,
    validator: Option<ProfileValidator>,
    in_flight: AtomicU32,
    poisoned: AtomicBool,
    counters: Counters,
}

impl<E: Engine> MotionJsProvider<E> {
    /// A JavaScript engine gets the admitted-syntax profile validator by default (sources outside the
    /// profile are refused before the engine runs); `without_validator` removes it.
    pub fn new(engine: E, quotas: Quotas) -> Self {
        let validator = (engine.source_language() == SourceLanguage::JavaScript)
            .then(|| ProfileValidator::for_max_bytes(quotas.max_source_bytes()));
        Self {
            engine,
            quotas,
            date: DateProfile::default(),
            clock: Arc::new(SystemClock::new()),
            owner: thread::current().id(),
            validator,
            in_flight: AtomicU32::new(0),
            poisoned: AtomicBool::new(false),
            counters: Counters::default(),
        }
    }

    /// Injects the persisted Date profile (MOT-256); the engine never sees an ambient clock.
    pub fn with_date(mut self, date: DateProfile) -> Self {
        self.date = date;
        self
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Replaces the profile validator (for example with other token/depth bounds).
    pub fn with_validator(mut self, validator: ProfileValidator) -> Self {
        self.validator = Some(validator);
        self
    }

    /// Removes the profile validator so source reaches the engine unchecked. Only for tests that
    /// attack the engine's own hardening directly; production callers keep the default.
    pub fn without_validator(mut self) -> Self {
        self.validator = None;
        self
    }

    pub fn validator(&self) -> Option<&ProfileValidator> {
        self.validator.as_ref()
    }

    pub fn engine_identity(&self) -> EngineIdentity {
        self.engine.identity()
    }

    /// The engine adapter (for engine-specific diagnostics such as worker-pool counters).
    pub fn engine(&self) -> &E {
        &self.engine
    }

    pub fn quotas(&self) -> &Quotas {
        &self.quotas
    }

    pub fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }

    pub fn stats(&self) -> ProviderStats {
        let c = &self.counters;
        let mut denials = [0; BudgetKind::COUNT];
        for (slot, counter) in denials.iter_mut().zip(&c.denials) {
            *slot = counter.load(Ordering::Acquire);
        }
        let mut faults = [0; EngineFault::COUNT];
        for (slot, counter) in faults.iter_mut().zip(&c.faults) {
            *slot = counter.load(Ordering::Acquire);
        }
        ProviderStats {
            evaluations: c.evaluations.load(Ordering::Acquire),
            realms_started: c.realms_started.load(Ordering::Acquire),
            realms_retired: c.realms_retired.load(Ordering::Acquire),
            charged_units: c.charged_units.load(Ordering::Acquire),
            checkpoints: c.checkpoints.load(Ordering::Acquire),
            host_reads: c.host_reads.load(Ordering::Acquire),
            validation_rejected: c.validation_rejected.load(Ordering::Acquire),
            denials,
            faults,
        }
    }

    /// Consumes the single owner and reports whether every realm and permit was retired on the
    /// owning thread.
    pub fn retire(self) -> RetireReport {
        let stats = self.stats();
        RetireReport {
            realms_started: stats.realms_started,
            realms_retired: stats.realms_retired,
            permits_released: self.in_flight.load(Ordering::Acquire) == 0,
            poisoned: self.is_poisoned(),
            owner_thread: thread::current().id() == self.owner,
        }
    }

    fn deny(&self, kind: BudgetKind) -> ExprError {
        self.counters.denials[kind.index()].fetch_add(1, Ordering::AcqRel);
        ExprError::new(ExprErrorCode::Budget)
    }

    /// An isolation fault (worker crash, protocol violation, spawn refusal) fails this evaluation
    /// only. Motion has no engine-fault code, so it maps to `Unsupported` (UNVERIFIED mapping); the
    /// typed cause is `ProviderStats::faulted` (and `EngineFailure::Faulted` at the engine API).
    fn fault(&self, kind: EngineFault) -> ExprError {
        self.counters.faults[kind.index()].fetch_add(1, Ordering::AcqRel);
        ExprError::new(ExprErrorCode::Unsupported)
    }

    /// Maps a profile violation to the typed Motion error with its exact position (STU-MOT-259).
    fn reject(&self, violation: ProfileViolation) -> ExprError {
        self.counters.validation_rejected.fetch_add(1, Ordering::AcqRel);
        let code = match violation.kind.class() {
            ViolationClass::Parse => ExprErrorCode::Parse,
            ViolationClass::Unsupported => ExprErrorCode::Unsupported,
            ViolationClass::Budget(kind) => {
                self.counters.denials[kind.index()].fetch_add(1, Ordering::AcqRel);
                ExprErrorCode::Budget
            }
        };
        ExprError {
            code,
            span: Some(violation.span),
        }
    }

    fn poison(&self) -> ExprError {
        self.poisoned.store(true, Ordering::Release);
        ExprError::new(ExprErrorCode::Unsupported)
    }

    fn acquire(&self) -> Result<Permit<'_>, ExprError> {
        let max = self.quotas.max_concurrent_realms();
        let mut current = self.in_flight.load(Ordering::Acquire);
        loop {
            if current >= max {
                return Err(self.deny(BudgetKind::Realms));
            }
            match self.in_flight.compare_exchange(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Permit(&self.in_flight)),
                Err(seen) => current = seen,
            }
        }
    }

    fn map_failure(&self, failure: EngineFailure) -> ExprError {
        match failure {
            EngineFailure::Parse(span) => ExprError { code: ExprErrorCode::Parse, span },
            EngineFailure::Unsupported(span) => ExprError { code: ExprErrorCode::Unsupported, span },
            EngineFailure::Thrown { kind: ThrownKind::Reference, span } => {
                ExprError { code: ExprErrorCode::ReferenceMissing, span }
            }
            EngineFailure::Thrown { kind: ThrownKind::Other, span } => {
                ExprError { code: ExprErrorCode::Unsupported, span }
            }
            EngineFailure::Budget(kind) => self.deny(kind),
            EngineFailure::Output => ExprError::new(ExprErrorCode::OutputType),
            EngineFailure::Host(error) => error,
            EngineFailure::Canceled => ExprError::canceled(),
            EngineFailure::Faulted(kind) => self.fault(kind),
            EngineFailure::Internal => self.poison(),
        }
    }

    fn limits(&self) -> EngineLimits {
        EngineLimits {
            memory_bytes: self.quotas.memory_bytes(),
            stack_bytes: self.quotas.stack_bytes(),
            recursion_limit: self.quotas.recursion_limit(),
            max_output_nodes: self.quotas.max_output_nodes(),
            max_output_depth: self.quotas.max_output_depth(),
        }
    }
}

struct Bridge<'a> {
    host: &'a dyn HostRead,
    fuel: &'a mut Fuel,
    cancel: &'a CancellationToken,
    clock: &'a dyn Clock,
    deadline_ns: u64,
    tick: EvalTick,
    date: &'a DateProfile,
    reads: u32,
    max_reads: u32,
    counters: &'a Counters,
}

fn host_failure(error: ExprError) -> EngineFailure {
    match error.code {
        ExprErrorCode::Budget => EngineFailure::Budget(BudgetKind::Fuel),
        ExprErrorCode::Canceled => EngineFailure::Canceled,
        _ => EngineFailure::Host(error),
    }
}

impl HostBridge for Bridge<'_> {
    fn fuel_remaining(&self) -> u64 {
        self.fuel.remaining()
    }

    fn checkpoint(&mut self) -> Result<(), EngineFailure> {
        self.counters.checkpoints.fetch_add(1, Ordering::AcqRel);
        self.cancel.check().map_err(|_| EngineFailure::Canceled)?;
        if self.clock.now_ns() > self.deadline_ns {
            return Err(EngineFailure::Budget(BudgetKind::Deadline));
        }
        Ok(())
    }

    fn grant_time(&mut self, ns: u64) {
        self.deadline_ns = self.deadline_ns.saturating_add(ns);
    }

    fn charge(&mut self, units: u64) -> Result<(), EngineFailure> {
        self.checkpoint()?;
        self.fuel
            .charge(units)
            .map_err(|_| EngineFailure::Budget(BudgetKind::Fuel))?;
        self.counters.charged_units.fetch_add(units, Ordering::AcqRel);
        Ok(())
    }

    fn read(&mut self, path: &str, tick: Option<EvalTick>) -> Result<Vec<f64>, EngineFailure> {
        self.checkpoint()?;
        if self.reads >= self.max_reads {
            return Err(EngineFailure::Budget(BudgetKind::HostReads));
        }
        self.reads += 1;
        let path = PropertyPath::new(path)
            .map_err(|_| EngineFailure::Host(ExprError::new(ExprErrorCode::ReferenceMissing)))?;
        self.counters.host_reads.fetch_add(1, Ordering::AcqRel);
        self.host
            .read(&path, tick.unwrap_or(self.tick), self.fuel)
            .map_err(host_failure)
    }

    fn utc_ms(&mut self) -> Result<i64, EngineFailure> {
        self.date
            .utc_ms(self.tick)
            .map_err(|_| EngineFailure::Host(ExprError::new(ExprErrorCode::Unsupported)))
    }
}

/// Iterative size check (no recursion on hostile depth), then shape check. No partial output.
fn convert_output(value: &JsValue, dimension: usize, nodes_cap: usize, depth_cap: usize) -> Result<Vec<f64>, ExprError> {
    let output = || ExprError::new(ExprErrorCode::OutputType);
    let mut nodes = 0_usize;
    let mut stack = vec![(value, 1_usize)];
    while let Some((node, depth)) = stack.pop() {
        nodes += 1;
        if nodes > nodes_cap || depth > depth_cap {
            return Err(output());
        }
        if let JsValue::Array(items) = node {
            if nodes.saturating_add(items.len()) > nodes_cap {
                return Err(output());
            }
            stack.extend(items.iter().map(|item| (item, depth + 1)));
        }
    }
    let numbers = match value {
        JsValue::Number(n) if dimension == 1 => vec![*n],
        JsValue::Array(items) if items.len() == dimension => items
            .iter()
            .map(|item| match item {
                JsValue::Number(n) => Ok(*n),
                _ => Err(output()),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(output()),
    };
    if numbers.iter().all(|n| n.is_finite()) {
        Ok(numbers)
    } else {
        Err(output())
    }
}

impl<E: Engine> ExpressionProvider for MotionJsProvider<E> {
    fn profile_id(&self) -> &'static str {
        PROFILE_ID
    }

    fn evaluate(
        &self,
        request: &ExprRequest<'_>,
        host: &dyn HostRead,
        fuel: &mut Fuel,
        cancel: &CancellationToken,
    ) -> Result<Vec<f64>, ExprError> {
        cancel.check().map_err(|_| ExprError::canceled())?;
        if self.is_poisoned() {
            return Err(ExprError::new(ExprErrorCode::Unsupported));
        }
        if request.source.len() > self.quotas.max_source_bytes() {
            return Err(self.deny(BudgetKind::SourceBytes));
        }
        if let Some(validator) = &self.validator
            && let Err(violation) = validator.validate(request.source)
        {
            return Err(self.reject(violation));
        }
        let _permit = self.acquire()?;
        self.counters.evaluations.fetch_add(1, Ordering::AcqRel);
        let deadline_ns = self
            .clock
            .now_ns()
            .saturating_add(u64::try_from(self.quotas.deadline().as_nanos()).unwrap_or(u64::MAX));
        let job = EvalJob {
            cancel: cancel.clone(),
            clock: Arc::clone(&self.clock),
            deadline_ns,
            source: request.source,
            dimension: request.dimension,
            base: request.base,
            revision: request.revision,
            tick: request.tick,
            seed: request.seed,
            limits: self.limits(),
        };
        let mut bridge = Bridge {
            host,
            fuel,
            cancel,
            clock: self.clock.as_ref(),
            deadline_ns,
            tick: request.tick,
            date: &self.date,
            reads: 0,
            max_reads: self.quotas.max_host_reads(),
            counters: &self.counters,
        };
        self.counters.realms_started.fetch_add(1, Ordering::AcqRel);
        let Ok(run) = catch_unwind(AssertUnwindSafe(|| self.engine.run(&job, &mut bridge))) else {
            // Engine panicked: realm state unknown, never counted as retired.
            return Err(self.poison());
        };
        if run.realm.dropped {
            self.counters.realms_retired.fetch_add(1, Ordering::AcqRel);
        }
        if !run.realm.fresh || !run.realm.dropped {
            return Err(self.poison());
        }
        let value = match run.outcome {
            Ok(value) => value,
            Err(failure) => return Err(self.map_failure(failure)),
        };
        cancel.check().map_err(|_| ExprError::canceled())?;
        convert_output(
            &value,
            request.dimension,
            self.quotas.max_output_nodes(),
            self.quotas.max_output_depth(),
        )
    }
}

/// Delivers the closed terminal outcome of one provider evaluation through a caller-owned sink.
/// No source text, values or paths reach the sink.
pub fn report_eval(
    result: &Result<Vec<f64>, ExprError>,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<Receipt, hsk_studio_observe::Error> {
    use hsk_studio_observe::FailureCode;
    let outcome = match result {
        Ok(_) => Outcome::Success,
        Err(error) => match error.code {
            ExprErrorCode::Canceled => Outcome::Canceled,
            ExprErrorCode::Unsupported => Outcome::Failure(FailureCode::Unsupported),
            _ => Outcome::Failure(FailureCode::Validation),
        },
    };
    observe.emit(
        Observation {
            correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        cancel,
        sink,
    )
}
