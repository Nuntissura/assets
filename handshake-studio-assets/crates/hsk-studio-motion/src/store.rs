//! `PropertyStore`: declared properties plus their expression read edges. Evaluates all
//! properties at one canonical tick in `DepGraph::eval_order` and serves the already-evaluated
//! values as the `HostRead` for cross-property reads.
//!
//! Policy (deterministic, cycle-safe, revision-aware):
//! * Read edges are committed through `DepGraph` (cycles refused, store unchanged), so every read
//!   target is evaluated before its reader and no read can recurse.
//! * An expression may only read paths in its declared read set; anything else is
//!   `reference_missing` (the order would not be guaranteed).
//! * A read at the canonical tick returns the target's evaluated value (including a fallback when
//!   its own expression was disabled). A read at any other tick returns the target's keyframed or
//!   static value; a target whose expression is active at that point is `unsupported` because
//!   expressions are never re-entered.
//! * Every read and every provider call charges the one shared `Fuel`.
//! * The store revision advances on every accepted structural or property mutation; a stale caller
//!   revision is rejected before anything is evaluated. A hard error (cancellation, out of bounds,
//!   stale property revision) aborts the whole tick with no partial result; every property stays
//!   in the store.
use crate::{
    depgraph::DepGraph,
    error::MotionError,
    expression::{
        ExprError, ExprErrorCode, ExpressionProvider, Fuel, HostRead,
    },
    path::PropertyPath,
    property::{EvalContext, Evaluation, Property},
    time::EvalTick,
};
use hsk_studio_accord::CancellationToken;
use std::collections::BTreeMap;

/// Canonical inputs of one store evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreContext {
    pub expected_revision: u64,
    pub tick: EvalTick,
    pub seed: u64,
}

/// All properties evaluated at one tick, in evaluation order.
#[derive(Clone, Debug, PartialEq)]
pub struct TickEvaluation {
    pub revision: u64,
    pub tick: EvalTick,
    pub seed: u64,
    pub results: Vec<(PropertyPath, Evaluation)>,
}

impl TickEvaluation {
    pub fn get(&self, path: &PropertyPath) -> Option<&Evaluation> {
        self.results
            .iter()
            .find(|(candidate, _)| candidate == path)
            .map(|(_, evaluation)| evaluation)
    }
}

#[derive(Debug, Default)]
pub struct PropertyStore {
    properties: BTreeMap<PropertyPath, Property>,
    graph: DepGraph,
    revision: u64,
}

struct TickView<'a> {
    tick: EvalTick,
    allowed: &'a [&'a PropertyPath],
    done: &'a BTreeMap<PropertyPath, Vec<f64>>,
    others: &'a BTreeMap<PropertyPath, Property>,
}

impl HostRead for TickView<'_> {
    fn read(
        &self,
        path: &PropertyPath,
        tick: EvalTick,
        fuel: &mut Fuel,
    ) -> Result<Vec<f64>, ExprError> {
        fuel.charge(1)?;
        if !self.allowed.contains(&path) {
            return Err(ExprError::new(ExprErrorCode::ReferenceMissing));
        }
        if tick == self.tick {
            return self
                .done
                .get(path)
                .cloned()
                .ok_or(ExprError::new(ExprErrorCode::ReferenceMissing));
        }
        let target = self
            .others
            .get(path)
            .ok_or(ExprError::new(ExprErrorCode::ReferenceMissing))?;
        if target.expression_active() {
            return Err(ExprError::new(ExprErrorCode::Unsupported));
        }
        target
            .sample(tick, &CancellationToken::default())
            .map_err(|_| ExprError::new(ExprErrorCode::OutputType))
    }
}

impl PropertyStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn len(&self) -> usize {
        self.properties.len()
    }

    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }

    pub fn get(&self, path: &PropertyPath) -> Option<&Property> {
        self.properties.get(path)
    }

    pub fn graph(&self) -> &DepGraph {
        &self.graph
    }

    /// Adds a property under a stable path. Advances the store revision.
    pub fn insert(&mut self, path: PropertyPath, property: Property) -> Result<(), MotionError> {
        if self.properties.contains_key(&path) {
            return Err(MotionError::DuplicateProperty);
        }
        self.graph.declare(path.clone())?;
        self.properties.insert(path, property);
        self.revision += 1;
        Ok(())
    }

    /// Commits the read set of `path`'s expression. A cycle or an unknown path is refused and
    /// leaves the store (and its revision) unchanged.
    pub fn set_reads(
        &mut self,
        path: &PropertyPath,
        reads: &[PropertyPath],
    ) -> Result<(), MotionError> {
        self.graph.commit_expression(path, reads)?;
        self.revision += 1;
        Ok(())
    }

    /// Mutates one property. The store revision advances only when `change` succeeds.
    pub fn update<R>(
        &mut self,
        path: &PropertyPath,
        change: impl FnOnce(&mut Property) -> Result<R, MotionError>,
    ) -> Result<R, MotionError> {
        let property = self
            .properties
            .get_mut(path)
            .ok_or(MotionError::UnknownProperty)?;
        let result = change(property)?;
        self.revision += 1;
        Ok(result)
    }

    /// Evaluates every property at `ctx.tick` in dependency order. Each property is evaluated
    /// against its own current revision (it cannot change mid-call); the store revision must match
    /// `ctx.expected_revision`.
    pub fn evaluate_tick(
        &mut self,
        ctx: &StoreContext,
        provider: &dyn ExpressionProvider,
        fuel: &mut Fuel,
        cancel: &CancellationToken,
    ) -> Result<TickEvaluation, MotionError> {
        cancel.check().map_err(|_| MotionError::Canceled)?;
        if ctx.expected_revision != self.revision {
            return Err(MotionError::StaleRevision {
                expected: ctx.expected_revision,
                actual: self.revision,
            });
        }
        let order = self.graph.eval_order();
        let mut done: BTreeMap<PropertyPath, Vec<f64>> = BTreeMap::new();
        let mut results = Vec::with_capacity(order.len());
        for path in order {
            let mut property = self
                .properties
                .remove(&path)
                .ok_or(MotionError::UnknownProperty)?;
            let allowed = self.graph.reads_of(&path);
            let result = {
                let view = TickView {
                    tick: ctx.tick,
                    allowed: &allowed,
                    done: &done,
                    others: &self.properties,
                };
                let property_ctx = EvalContext {
                    expected_revision: property.revision(),
                    tick: ctx.tick,
                    seed: ctx.seed,
                };
                property.evaluate_with(&property_ctx, provider, &view, fuel, cancel)
            };
            self.properties.insert(path.clone(), property);
            let evaluation = result?;
            done.insert(path.clone(), evaluation.value.clone());
            results.push((path, evaluation));
        }
        Ok(TickEvaluation {
            revision: self.revision,
            tick: ctx.tick,
            seed: ctx.seed,
            results,
        })
    }
}
