use crate::*;
use kumihimo_core::{FrameId, GeometryOutput};
use serde::{Deserialize, Serialize};

/// Host's physical desktop origin and logical-to-physical scale; no arrangement store.
#[derive(Clone, Copy, Debug)]
pub struct DesktopMapping { pub origin_x: i32, pub origin_y: i32, pub pixels_per_dip: f64 }
impl DesktopMapping {
    pub fn project(self, geometry: &GeometryOutput, frame: FrameId) -> Result<PixelBounds, Diagnostic> {
        let rect = geometry.frames.get(&frame).ok_or_else(|| Diagnostic::new("missing_frame", "Frame is absent from canonical geometry"))?;
        if !self.pixels_per_dip.is_finite() || self.pixels_per_dip <= 0.0 {
            return Err(Diagnostic::new("invalid_scale", "Scale must be finite and positive"));
        }
        let convert = |value: f64| -> Result<i32, Diagnostic> {
            let value = value.round();
            if !value.is_finite() || value < i32::MIN as f64 || value > i32::MAX as f64 {
                Err(Diagnostic::new("coordinate_overflow", "Projected coordinate exceeds physical desktop range"))
            } else { Ok(value as i32) }
        };
        let x = convert(self.origin_x as f64 + rect.x() * self.pixels_per_dip)?;
        let y = convert(self.origin_y as f64 + rect.y() * self.pixels_per_dip)?;
        let right = convert(self.origin_x as f64 + rect.right() * self.pixels_per_dip)?;
        let bottom = convert(self.origin_y as f64 + rect.bottom() * self.pixels_per_dip)?;
        let bounds = PixelBounds { x, y, width: right.checked_sub(x).ok_or_else(|| Diagnostic::new("coordinate_overflow", "Width overflow"))?, height: bottom.checked_sub(y).ok_or_else(|| Diagnostic::new("coordinate_overflow", "Height overflow"))? };
        bounds.validate()?;
        Ok(bounds)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ObserveBudget { pub attempts: u8, pub interval_ms: u64 }
impl Default for ObserveBudget { fn default() -> Self { Self { attempts: 20, interval_ms: 25 } } }
impl ObserveBudget {
    fn validate(self) -> Result<(), Diagnostic> {
        if self.attempts == 0 || self.attempts > 40 || self.interval_ms > 100 {
            Err(Diagnostic::new("invalid_budget", "Observation requires 1..40 attempts with 0..100ms intervals"))
        } else { Ok(()) }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectOutcome { Applied, Unchanged, Rejected, Failed, Partial, Unknown }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectReceipt {
    pub semantic_reference: String,
    pub requested: Option<Placement>,
    pub before: Option<Placement>,
    pub observed: Option<Placement>,
    pub outcome: EffectOutcome,
    pub request_error: Option<Diagnostic>,
    pub observation_error: Option<Diagnostic>,
    pub compensation: Option<Box<EffectReceipt>>,
    /// Unsettled dispatch must be reconciled before any new effect or detach success.
    pub pending: Option<Placement>,
}

pub struct ExternalApps<B: WindowBackend> { backend: B }
impl<B: WindowBackend> ExternalApps<B> {
    pub fn new(backend: B) -> Self { Self { backend } }
    pub fn backend_mut(&mut self) -> &mut B { &mut self.backend }
    pub fn bind(&mut self, target: WindowTarget, semantic_reference: &str) -> Result<SelectedWindow<B::Lease>, Diagnostic> {
        if semantic_reference.is_empty() || semantic_reference.len() > 512 {
            return Err(Diagnostic::new("invalid_reference", "Semantic reference requires 1..512 UTF-8 bytes"));
        }
        let (lease, identity) = self.backend.bind(target)?;
        self.backend.validate(&lease, identity)?;
        let original = self.backend.observe(&lease)?;
        original.bounds.validate()?;
        if original.state != WindowState::Normal { return Err(Diagnostic::new("unsupported_initial_state", "Binding requires a normal window with quietly restorable placement")); }
        Ok(SelectedWindow { lease, identity, semantic_reference: semantic_reference.into(), original, pending: std::cell::Cell::new(None) })
    }
    pub fn inspect(&mut self, selected: &SelectedWindow<B::Lease>) -> Result<(Placement, Capabilities), Diagnostic> {
        self.backend.validate(&selected.lease, selected.identity)?;
        Ok((self.backend.observe(&selected.lease)?, self.backend.capabilities(&selected.lease)?))
    }
    fn rejected(selected: &SelectedWindow<B::Lease>, error: Diagnostic) -> EffectReceipt {
        EffectReceipt { semantic_reference: selected.semantic_reference.clone(), requested: None, before: None, observed: None, outcome: EffectOutcome::Rejected, request_error: Some(error), observation_error: None, compensation: None, pending:selected.pending() }
    }
    pub fn tile(&mut self, selected: &SelectedWindow<B::Lease>, geometry: &GeometryOutput, frame: FrameId, mapping: DesktopMapping, budget: ObserveBudget) -> EffectReceipt {
        let preparation = (|| {
            budget.validate()?;
            let bounds = mapping.project(geometry, frame)?;
            let (before, caps) = self.inspect(selected)?;
            if before.state != WindowState::Normal { return Err(Diagnostic::new("unsupported_state", "Quiet tiling supports normal windows; minimized/maximized state is unchanged")); }
            if !caps.can_resize || bounds.width < caps.minimum_width || bounds.height < caps.minimum_height {
                return Err(Diagnostic::new("unsupported_size", "Window cannot resize to the selected allocation"));
            }
            if !caps.work_area.contains(bounds) { return Err(Diagnostic::new("outside_work_area", "Allocation must fit the selected window's current monitor work area")); }
            if (mapping.pixels_per_dip - caps.dpi as f64 / 96.0).abs() > 0.001 {
                return Err(Diagnostic::new("dpi_mismatch", "Host scale does not match selected window DPI; cross-monitor tiling is unsupported"));
            }
            Ok((before, Placement { bounds, state: WindowState::Normal }))
        })();
        let (before, desired) = match preparation { Ok(value) => value, Err(error) => return Self::rejected(selected, error) };
        let mut receipt = self.effect(selected, before, desired, budget, false);
        if receipt.outcome == EffectOutcome::Partial && receipt.pending.is_none() {
            // One bounded compensation to the pre-operation placement, never recursive retries.
            receipt.compensation = Some(Box::new(self.restore_to(selected, before, budget)));
        }
        receipt
    }
    pub fn restore(&mut self, selected: &SelectedWindow<B::Lease>, budget: ObserveBudget) -> EffectReceipt { self.restore_to(selected, selected.original, budget) }
    fn restore_to(&mut self, selected: &SelectedWindow<B::Lease>, desired: Placement, budget: ObserveBudget) -> EffectReceipt {
        if let Err(error) = budget.validate().and_then(|_| desired.bounds.validate()) { return Self::rejected(selected, error); }
        if let Err(error) = self.backend.validate(&selected.lease, selected.identity) { return Self::rejected(selected, error); }
        match self.backend.observe(&selected.lease) {
            Ok(before) => self.effect(selected, before, desired, budget, true),
            Err(error) => Self::rejected(selected, error),
        }
    }
    fn effect(&mut self, selected: &SelectedWindow<B::Lease>, before: Placement, desired: Placement, budget: ObserveBudget, restore: bool) -> EffectReceipt {
        let mut result = EffectReceipt { semantic_reference: selected.semantic_reference.clone(), requested: Some(desired), before: Some(before), observed: Some(before), outcome: EffectOutcome::Unchanged, request_error: None, observation_error: None, compensation: None, pending:selected.pending() };
        if let Some(pending) = selected.pending() {
            // An unchanged window does not prove that an earlier queued request is finished.
            for attempt in 0..budget.attempts {
                if attempt != 0 { self.backend.observation_pause(budget.interval_ms); }
                let observation = self.backend.validate(&selected.lease, selected.identity)
                    .and_then(|_| self.backend.observe(&selected.lease));
                match observation {
                    Ok(observed) => {
                        result.observed = Some(observed);
                        match self.backend.request_completed(&selected.lease) {
                            Ok(completed) if completed || observed == pending => { selected.pending.set(None); result.pending = None; break; },
                            Ok(_) => {},
                            Err(error) => { result.observation_error = Some(error); result.outcome = EffectOutcome::Unknown; return result; }
                        }
                    },
                    Err(error) => { result.observation_error = Some(error); result.outcome = EffectOutcome::Unknown; return result; }
                }
            }
            if selected.pending().is_some() {
                result.outcome = EffectOutcome::Unknown;
                result.observation_error = Some(Diagnostic::new("pending_dispatch", "Prior OS request remains unsettled; no new effect or successful detach is permitted"));
                return result;
            }
            result.before = result.observed;
            if result.observed == Some(desired) { return result; }
        }
        if result.observed == Some(desired) { return result; }
        if let Err(error) = self.backend.validate(&selected.lease, selected.identity) {
            result.outcome = EffectOutcome::Rejected; result.request_error = Some(error); return result;
        }
        selected.pending.set(Some(desired)); result.pending = Some(desired);
        result.request_error = if restore { self.backend.request_restore(&selected.lease, desired).err() } else { self.backend.request_bounds(&selected.lease, desired.bounds).err() };
        for attempt in 0..budget.attempts {
            if attempt != 0 { self.backend.observation_pause(budget.interval_ms); }
            if let Err(error) = self.backend.validate(&selected.lease, selected.identity) {
                result.observation_error = Some(error); result.outcome = EffectOutcome::Unknown; return result;
            }
            match self.backend.observe(&selected.lease) {
                Ok(observed) => { result.observed = Some(observed); if observed == desired { selected.pending.set(None); result.pending = None; result.outcome = EffectOutcome::Applied; return result; } },
                Err(error) => { result.observation_error = Some(error); result.outcome = EffectOutcome::Unknown; return result; }
            }
        }
        match self.backend.request_completed(&selected.lease) {
            Ok(true) => { selected.pending.set(None); result.pending = None; result.outcome = if result.observed == result.before { EffectOutcome::Failed } else { EffectOutcome::Partial }; },
            Ok(false) => { result.outcome = if result.observed == result.before { EffectOutcome::Unknown } else { EffectOutcome::Partial }; result.observation_error = Some(Diagnostic::new("pending_dispatch", "Request has no completion witness; recovery must reconcile it before another effect")); },
            Err(error) => { result.outcome = EffectOutcome::Unknown; result.observation_error = Some(error); },
        }
        result
    }
}
