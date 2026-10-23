//! Typed expression-provider ports (STU-MOT-070/071/076/078, MOT-255..259). No engine, no JS types:
//! a provider implements `ExpressionProvider` outside this crate (MotionJS owns the JS profile).
//! Pure Motion resolves without any provider: `NoProvider` reports `Unsupported` and the property
//! falls back to its underlying keyframed/static value.
use crate::{path::PropertyPath, time::EvalTick};
use hsk_studio_accord::CancellationToken;

/// MOT-259 stable typed outcome vocabulary, plus `Canceled` (never disables an expression).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExprErrorCode {
    Parse,
    Unsupported,
    Alias,
    ReferenceMissing,
    Ambiguous,
    DependencyCycle,
    Budget,
    OutputType,
    Canceled,
}

impl ExprErrorCode {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::Unsupported => "unsupported",
            Self::Alias => "alias",
            Self::ReferenceMissing => "reference_missing",
            Self::Ambiguous => "ambiguous",
            Self::DependencyCycle => "dependency_cycle",
            Self::Budget => "budget",
            Self::OutputType => "output_type",
            Self::Canceled => "canceled",
        }
    }
}

/// 1-based source position inside the expression text (STU-MOT-071: error source position).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceSpan {
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExprError {
    pub code: ExprErrorCode,
    pub span: Option<SourceSpan>,
}

impl ExprError {
    pub const fn new(code: ExprErrorCode) -> Self {
        Self { code, span: None }
    }

    pub const fn at(code: ExprErrorCode, line: u32, column: u32) -> Self {
        Self {
            code,
            span: Some(SourceSpan { line, column }),
        }
    }

    pub const fn canceled() -> Self {
        Self::new(ExprErrorCode::Canceled)
    }
}

/// Shared evaluation allowance across a property, its nested host reads and retries. Exhaustion is
/// permanent for this allowance, so the outcome stays deterministic for a given limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fuel {
    limit: u64,
    used: u64,
}

impl Fuel {
    pub const fn new(limit: u64) -> Self {
        Self { limit, used: 0 }
    }

    pub const fn used(&self) -> u64 {
        self.used
    }

    pub const fn remaining(&self) -> u64 {
        self.limit - self.used
    }

    pub fn charge(&mut self, units: u64) -> Result<(), ExprError> {
        match self.used.checked_add(units) {
            Some(total) if total <= self.limit => {
                self.used = total;
                Ok(())
            }
            _ => {
                self.used = self.limit;
                Err(ExprError::new(ExprErrorCode::Budget))
            }
        }
    }
}

/// Immutable inputs a provider may read (MOT-255): revision, canonical evaluation tick, explicit
/// seed. `base` is the keyframed value for `expression_over_keyframes`, `None` otherwise.
#[derive(Clone, Copy, Debug)]
pub struct ExprRequest<'a> {
    pub source: &'a str,
    pub revision: u64,
    pub tick: EvalTick,
    pub seed: u64,
    pub dimension: usize,
    pub base: Option<&'a [f64]>,
}

/// Host-owned read of another property at a tick (cross-property reference). The reader must be
/// deterministic and side-effect free; it shares the caller's `Fuel`.
pub trait HostRead: Send + Sync {
    fn read(
        &self,
        path: &PropertyPath,
        tick: EvalTick,
        fuel: &mut Fuel,
    ) -> Result<Vec<f64>, ExprError>;
}

/// Synchronous, deterministic, side-effect-free expression evaluator (no wall clock, no I/O).
pub trait ExpressionProvider: Send + Sync {
    /// Stable profile identifier recorded in receipts.
    fn profile_id(&self) -> &'static str;

    fn evaluate(
        &self,
        request: &ExprRequest<'_>,
        host: &dyn HostRead,
        fuel: &mut Fuel,
        cancel: &CancellationToken,
    ) -> Result<Vec<f64>, ExprError>;
}

/// Pure-Rust closure: no expression engine present.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoProvider;

impl ExpressionProvider for NoProvider {
    fn profile_id(&self) -> &'static str {
        "none"
    }

    fn evaluate(
        &self,
        _request: &ExprRequest<'_>,
        _host: &dyn HostRead,
        _fuel: &mut Fuel,
        _cancel: &CancellationToken,
    ) -> Result<Vec<f64>, ExprError> {
        Err(ExprError::new(ExprErrorCode::Unsupported))
    }
}

/// Host with no readable properties.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoHost;

impl HostRead for NoHost {
    fn read(
        &self,
        _path: &PropertyPath,
        _tick: EvalTick,
        _fuel: &mut Fuel,
    ) -> Result<Vec<f64>, ExprError> {
        Err(ExprError::new(ExprErrorCode::ReferenceMissing))
    }
}
