//! `StudioProperty` value stream (STU-MOT-010/010a/020b): static value retained under animation,
//! keyframes, optional expression, four-state machine, deterministic fail-closed evaluation.
use crate::{
    error::MotionError,
    expression::{
        ExprError, ExprErrorCode, ExprRequest, ExpressionProvider, Fuel, HostRead, NoHost,
        NoProvider,
    },
    keyframe::{
        Keyframe, MAX_DIMENSION, MAX_KEYFRAMES, sample_segment, validate_key, validate_segment,
    },
    path::validate_key as validate_segment_key,
    receipt::{EvalReceipt, PROFILE_KEYFRAMES},
    time::EvalTick,
};
use hsk_studio_accord::CancellationToken;

pub const MAX_EXPRESSION_BYTES: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropertyState {
    Static,
    Keyframed,
    ExpressionDriven,
    ExpressionOverKeyframes,
}

impl PropertyState {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Keyframed => "keyframed",
            Self::ExpressionDriven => "expression_driven",
            Self::ExpressionOverKeyframes => "expression_over_keyframes",
        }
    }
}

/// Canonical evaluation inputs: caller's expected revision (stale = rejected), exact tick, seed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvalContext {
    pub expected_revision: u64,
    pub tick: EvalTick,
    pub seed: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Evaluation {
    pub value: Vec<f64>,
    pub receipt: EvalReceipt,
}

#[derive(Clone, Debug)]
pub struct Property {
    key: String,
    static_value: Vec<f64>,
    keys: Vec<Keyframe>,
    key_ticks: Vec<i64>,
    interpolable: bool,
    hard_min: Option<f64>,
    hard_max: Option<f64>,
    expression: Option<String>,
    expression_enabled: bool,
    disabled: Option<ExprError>,
    revision: u64,
}

impl Property {
    /// `static_value` fixes the component count. `keys` must have unique strictly increasing times
    /// that fit `i64`, matching component counts and, on Bezier-family sides, tangents.
    pub fn new(
        key: &str,
        static_value: Vec<f64>,
        keys: Vec<Keyframe>,
    ) -> Result<Self, MotionError> {
        validate_segment_key(key)?;
        let mut property = Self {
            key: key.to_owned(),
            static_value,
            keys,
            key_ticks: Vec::new(),
            interpolable: true,
            hard_min: None,
            hard_max: None,
            expression: None,
            expression_enabled: false,
            disabled: None,
            revision: 0,
        };
        property.revalidate()?;
        Ok(property)
    }

    /// Hard limits: values outside are reported as `OutOfBounds`, never silently clamped.
    pub fn with_bounds(mut self, min: Option<f64>, max: Option<f64>) -> Result<Self, MotionError> {
        self.hard_min = min;
        self.hard_max = max;
        self.revalidate()?;
        Ok(self)
    }

    /// MOT-031b: a non-interpolable property behaves as `hold` on every segment.
    pub fn with_interpolable(mut self, interpolable: bool) -> Result<Self, MotionError> {
        self.interpolable = interpolable;
        self.revalidate()?;
        Ok(self)
    }

    /// Installs expression text enabled; revision advances. Clears any earlier disable-by-error.
    pub fn with_expression(mut self, source: &str) -> Result<Self, MotionError> {
        self.set_expression(Some(source))?;
        Ok(self)
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn dimension(&self) -> usize {
        self.static_value.len()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn static_value(&self) -> &[f64] {
        &self.static_value
    }

    pub fn keyframes(&self) -> &[Keyframe] {
        &self.keys
    }

    pub fn expression(&self) -> Option<&str> {
        self.expression.as_deref()
    }

    pub fn expression_enabled(&self) -> bool {
        self.expression_enabled
    }

    pub fn disabled_by_error(&self) -> Option<ExprError> {
        self.disabled
    }

    fn revalidate(&mut self) -> Result<(), MotionError> {
        let dimension = self.static_value.len();
        if dimension == 0 || dimension > MAX_DIMENSION {
            return Err(MotionError::DimensionLimit);
        }
        if self.keys.len() > MAX_KEYFRAMES {
            return Err(MotionError::KeyframeLimit);
        }
        if self.static_value.iter().any(|v| !v.is_finite())
            || self.hard_min.is_some_and(|v| !v.is_finite())
            || self.hard_max.is_some_and(|v| !v.is_finite())
        {
            return Err(MotionError::NonFinite);
        }
        if let (Some(min), Some(max)) = (self.hard_min, self.hard_max)
            && min > max
        {
            return Err(MotionError::InvalidBounds);
        }
        self.check_bounds(&self.static_value)?;
        let mut ticks = Vec::with_capacity(self.keys.len());
        for key in &self.keys {
            validate_key(key, dimension)?;
            self.check_bounds(&key.value)?;
            ticks.push(EvalTick::from_stored(key.time)?.value());
        }
        for pair in ticks.windows(2) {
            if pair[0] == pair[1] {
                return Err(MotionError::DuplicateKeyTime);
            }
            if pair[0] > pair[1] {
                return Err(MotionError::UnsortedKeys);
            }
        }
        for (index, pair) in self.keys.windows(2).enumerate() {
            validate_segment(
                &pair[0],
                &pair[1],
                ticks[index + 1] - ticks[index],
                self.interpolable,
            )?;
        }
        self.key_ticks = ticks;
        Ok(())
    }

    fn check_bounds(&self, value: &[f64]) -> Result<(), MotionError> {
        for component in value {
            if !component.is_finite() {
                return Err(MotionError::NonFinite);
            }
            if self.hard_min.is_some_and(|min| *component < min)
                || self.hard_max.is_some_and(|max| *component > max)
            {
                return Err(MotionError::OutOfBounds);
            }
        }
        Ok(())
    }

    /// Replaces the static value (component count must not change). Advances the revision.
    pub fn set_static_value(&mut self, value: Vec<f64>) -> Result<(), MotionError> {
        if value.len() != self.static_value.len() {
            return Err(MotionError::ComponentMismatch);
        }
        let previous = std::mem::replace(&mut self.static_value, value);
        if let Err(error) = self.revalidate() {
            self.static_value = previous;
            return Err(error);
        }
        self.revision += 1;
        Ok(())
    }

    /// Installs or removes expression text (enabled on install). Advances the revision, clears a
    /// previous disable-by-error.
    pub fn set_expression(&mut self, source: Option<&str>) -> Result<(), MotionError> {
        if source.is_some_and(|text| text.len() > MAX_EXPRESSION_BYTES) {
            return Err(MotionError::ExpressionTooLong);
        }
        self.expression = source.map(str::to_owned);
        self.expression_enabled = source.is_some();
        self.disabled = None;
        self.revision += 1;
        Ok(())
    }

    /// Enables or disables the expression; disabling all expression/keys returns to the retained
    /// static value (MOT-020b). Clears a previous disable-by-error and advances the revision.
    pub fn set_expression_enabled(&mut self, enabled: bool) {
        self.expression_enabled = enabled && self.expression.is_some();
        self.disabled = None;
        self.revision += 1;
    }

    /// State as configured, ignoring a disable-by-error.
    pub fn configured_state(&self) -> PropertyState {
        let expression = self.expression.is_some() && self.expression_enabled;
        match (expression, self.keys.is_empty()) {
            (true, true) => PropertyState::ExpressionDriven,
            (true, false) => PropertyState::ExpressionOverKeyframes,
            (false, true) => PropertyState::Static,
            (false, false) => PropertyState::Keyframed,
        }
    }

    /// Effective state: an expression disabled by error no longer drives the value (MOT-071).
    pub fn state(&self) -> PropertyState {
        if self.disabled.is_some() {
            if self.keys.is_empty() {
                PropertyState::Static
            } else {
                PropertyState::Keyframed
            }
        } else {
            self.configured_state()
        }
    }

    fn base_value(&self, tick: i64) -> Result<Vec<f64>, MotionError> {
        let (Some(first), Some(last)) = (self.keys.first(), self.keys.last()) else {
            return Ok(self.static_value.clone());
        };
        let after = self.key_ticks.partition_point(|time| *time <= tick);
        let value = if after == 0 {
            first.value.clone()
        } else if after == self.keys.len() {
            last.value.clone()
        } else {
            sample_segment(
                &self.keys[after - 1],
                &self.keys[after],
                self.key_ticks[after - 1],
                self.key_ticks[after],
                tick,
                self.interpolable,
            )?
        };
        Ok(value)
    }

    /// Keyframe/static value at `tick`, ignoring any expression. Before the first key and after the
    /// last key the end values are held. Result is finite and within hard bounds or an error.
    pub fn sample(
        &self,
        tick: EvalTick,
        cancel: &CancellationToken,
    ) -> Result<Vec<f64>, MotionError> {
        cancel.check().map_err(|_| MotionError::Canceled)?;
        let value = self.base_value(tick.value())?;
        self.check_bounds(&value)?;
        Ok(value)
    }

    /// Full evaluation without any expression engine (pure closure): a configured expression is
    /// disabled with `unsupported` and the underlying value is returned.
    pub fn evaluate_pure(
        &mut self,
        ctx: &EvalContext,
        cancel: &CancellationToken,
    ) -> Result<Evaluation, MotionError> {
        // One unit pays for the single provider call; `NoProvider` itself spends nothing.
        self.evaluate_with(ctx, &NoProvider, &NoHost, &mut Fuel::new(1), cancel)
    }

    /// STU-MOT-071/078 evaluation: stale revision rejected; expression error (any code except
    /// cancellation) disables the expression, falls back to the underlying keyframed/static value
    /// and reports `disabled_by_error` in the receipt; cancellation and out-of-bounds are errors
    /// and never produce a partial value.
    pub fn evaluate_with(
        &mut self,
        ctx: &EvalContext,
        provider: &dyn ExpressionProvider,
        host: &dyn HostRead,
        fuel: &mut Fuel,
        cancel: &CancellationToken,
    ) -> Result<Evaluation, MotionError> {
        cancel.check().map_err(|_| MotionError::Canceled)?;
        if ctx.expected_revision != self.revision {
            return Err(MotionError::StaleRevision {
                expected: ctx.expected_revision,
                actual: self.revision,
            });
        }
        let fuel_before = fuel.used();
        let base = self.sample(ctx.tick, cancel)?;
        let mut profile = PROFILE_KEYFRAMES;
        let mut value = base.clone();
        if self.expression.is_some() && self.expression_enabled && self.disabled.is_none() {
            profile = provider.profile_id();
            let base_for_expression = (!self.keys.is_empty()).then_some(base.as_slice());
            let source = self.expression.as_deref().unwrap_or_default();
            let request = ExprRequest {
                source,
                revision: self.revision,
                tick: ctx.tick,
                seed: ctx.seed,
                dimension: self.dimension(),
                base: base_for_expression,
            };
            let outcome = fuel
                .charge(1)
                .and_then(|()| provider.evaluate(&request, host, fuel, cancel))
                .and_then(|out| {
                    if out.len() == request.dimension && out.iter().all(|v| v.is_finite()) {
                        Ok(out)
                    } else {
                        Err(ExprError::new(ExprErrorCode::OutputType))
                    }
                });
            match outcome {
                Ok(out) => {
                    self.check_bounds(&out)?;
                    value = out;
                }
                Err(error) if error.code == ExprErrorCode::Canceled => {
                    return Err(MotionError::Canceled);
                }
                Err(error) => self.disabled = Some(error),
            }
            cancel.check().map_err(|_| MotionError::Canceled)?;
        }
        Ok(Evaluation {
            value,
            receipt: EvalReceipt {
                revision: self.revision,
                tick: ctx.tick,
                seed: ctx.seed,
                profile,
                state: self.state(),
                disabled_by_error: self.disabled.map(|error| error.code),
                error_span: self.disabled.and_then(|error| error.span),
                fuel_used: fuel.used() - fuel_before,
            },
        })
    }
}
