//! Lifecycle intent is separate from independent process observations.
use crate::frame::{BridgeError, BridgeLimits, ContentScope, Result, ScopeTarget};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub starttime: u64,
}
impl ProcessIdentity {
    pub fn validate(self) -> Result<()> {
        if self.pid == 0 || self.starttime == 0 {
            return Err(BridgeError::Invalid("process identity"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineState {
    Visible,
    Hidden,
    UnloadRequested,
    Unknown,
    Unloaded,
    Restoring,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestorationStrength {
    PageReload,
}

/// The host must obtain this from an independent kernel inspector, never the worker.
/// This value validates attribution; it does not itself inspect the OS or authenticate its producer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndependentProcessReceipt {
    pub scope: ContentScope,
    pub engine_incarnation: u64,
    pub process: ProcessIdentity,
    pub operation_id: u64,
    pub observed_ms: u64,
    pub inspection_id: String,
    pub process_absent: bool,
    pub descendants_absent: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineRecord {
    pub scope: ContentScope,
    pub engine_incarnation: u64,
    pub process: ProcessIdentity,
    pub page_reference: String,
    pub keep_alive: bool,
    pub dirty: bool,
    pub state: EngineState,
    pub operation_id: u64,
    pub requested_ms: u64,
    visible: bool,
    restore_attempts: u8,
    last_retry_basis: Option<String>,
}

#[derive(Clone, Debug)]
pub struct LifecycleManager {
    max_live: usize,
    engines: BTreeMap<ScopeTarget, EngineRecord>,
}
impl LifecycleManager {
    pub fn new(limits: &BridgeLimits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            max_live: limits.max_live_engines,
            engines: BTreeMap::new(),
        })
    }
    fn live_count(&self) -> usize {
        self.engines
            .values()
            .filter(|v| v.state != EngineState::Unloaded)
            .count()
    }
    pub fn register(
        &mut self,
        scope: ContentScope,
        engine_incarnation: u64,
        process: ProcessIdentity,
        page_reference: String,
        visible: bool,
    ) -> Result<()> {
        scope.validate()?;
        process.validate()?;
        if engine_incarnation == 0 {
            return Err(BridgeError::Invalid("engine incarnation"));
        }
        if page_reference.is_empty() || page_reference.len() > 4096 {
            return Err(BridgeError::Invalid("page reference"));
        }
        if self.engines.contains_key(&scope.target) {
            return Err(BridgeError::Duplicate);
        }
        if self.live_count() >= self.max_live {
            return Err(BridgeError::Budget("live engines"));
        }
        if self
            .engines
            .values()
            .any(|v| v.state != EngineState::Unloaded && v.process.pid == process.pid)
        {
            return Err(BridgeError::Invalid("shared browser process"));
        }
        self.engines.insert(
            scope.target,
            EngineRecord {
                scope,
                engine_incarnation,
                process,
                page_reference,
                keep_alive: false,
                dirty: false,
                state: if visible {
                    EngineState::Visible
                } else {
                    EngineState::Hidden
                },
                operation_id: 0,
                requested_ms: 0,
                visible,
                restore_attempts: 0,
                last_retry_basis: None,
            },
        );
        Ok(())
    }
    pub fn record(&self, target: ScopeTarget) -> Result<&EngineRecord> {
        self.engines.get(&target).ok_or(BridgeError::UnknownView)
    }
    pub fn protect(&mut self, target: ScopeTarget, keep_alive: bool, dirty: bool) -> Result<()> {
        let v = self
            .engines
            .get_mut(&target)
            .ok_or(BridgeError::UnknownView)?;
        v.keep_alive = keep_alive;
        v.dirty = dirty;
        Ok(())
    }
    /// Hiding controls host captures only; no suspension or resource-release claim.
    pub fn set_visible(&mut self, target: ScopeTarget, visible: bool) -> Result<()> {
        let v = self
            .engines
            .get_mut(&target)
            .ok_or(BridgeError::UnknownView)?;
        v.visible = visible;
        if matches!(v.state, EngineState::Visible | EngineState::Hidden) {
            v.state = if visible {
                EngineState::Visible
            } else {
                EngineState::Hidden
            };
        }
        Ok(())
    }
    pub fn request_unload(
        &mut self,
        target: ScopeTarget,
        operation_id: u64,
        now_ms: u64,
    ) -> Result<ProcessIdentity> {
        let v = self
            .engines
            .get_mut(&target)
            .ok_or(BridgeError::UnknownView)?;
        if matches!(
            v.state,
            EngineState::Unknown | EngineState::UnloadRequested | EngineState::Restoring
        ) {
            return Err(BridgeError::ReconciliationRequired);
        }
        if v.state == EngineState::Unloaded {
            return Err(BridgeError::Duplicate);
        }
        if v.keep_alive || v.dirty {
            return Err(BridgeError::Protected);
        }
        if operation_id == 0 || operation_id <= v.operation_id {
            return Err(BridgeError::Duplicate);
        }
        v.operation_id = operation_id;
        v.requested_ms = now_ms;
        v.state = EngineState::UnloadRequested;
        Ok(v.process)
    }
    /// A timeout/worker response does not establish process termination.
    pub fn outcome_unknown(&mut self, target: ScopeTarget, operation_id: u64) -> Result<()> {
        let v = self
            .engines
            .get_mut(&target)
            .ok_or(BridgeError::UnknownView)?;
        if v.operation_id != operation_id
            || !matches!(
                v.state,
                EngineState::UnloadRequested | EngineState::Restoring
            )
        {
            return Err(BridgeError::Stale("lifecycle operation"));
        }
        v.state = EngineState::Unknown;
        Ok(())
    }
    fn check_receipt(v: &EngineRecord, receipt: &IndependentProcessReceipt) -> Result<()> {
        receipt.scope.validate()?;
        receipt.process.validate()?;
        if receipt.inspection_id.is_empty() || receipt.inspection_id.len() > 128 {
            return Err(BridgeError::Invalid("independent inspection ID"));
        }
        if receipt.scope != v.scope
            || receipt.process != v.process
            || receipt.engine_incarnation != v.engine_incarnation
            || receipt.operation_id != v.operation_id
            || receipt.observed_ms < v.requested_ms
        {
            return Err(BridgeError::Stale("independent lifecycle receipt"));
        }
        if !matches!(
            v.state,
            EngineState::UnloadRequested | EngineState::Unknown | EngineState::Restoring
        ) {
            return Err(BridgeError::Invalid("receipt outside reconciliation"));
        }
        Ok(())
    }
    pub fn confirm_unloaded(&mut self, receipt: &IndependentProcessReceipt) -> Result<()> {
        let v = self
            .engines
            .get_mut(&receipt.scope.target)
            .ok_or(BridgeError::UnknownView)?;
        Self::check_receipt(v, receipt)?;
        if !receipt.process_absent || !receipt.descendants_absent {
            return Err(BridgeError::ReconciliationRequired);
        }
        v.state = EngineState::Unloaded;
        Ok(())
    }
    /// Independent presence reconciliation returns to a live, recoverable state.
    pub fn reconcile_live(&mut self, receipt: &IndependentProcessReceipt) -> Result<()> {
        let v = self
            .engines
            .get_mut(&receipt.scope.target)
            .ok_or(BridgeError::UnknownView)?;
        Self::check_receipt(v, receipt)?;
        if receipt.process_absent {
            return Err(BridgeError::ReconciliationRequired);
        }
        v.state = if v.visible {
            EngineState::Visible
        } else {
            EngineState::Hidden
        };
        Ok(())
    }
    /// Invoke only AFTER the host's fresh independent namespace admission succeeds.
    /// Fresh owned process identity is supplied by the host; no process is started here.
    pub fn begin_restore(
        &mut self,
        target: ScopeTarget,
        operation_id: u64,
        engine_incarnation: u64,
        process: ProcessIdentity,
        now_ms: u64,
        changed_input_or_cause: &str,
    ) -> Result<RestorationStrength> {
        process.validate()?;
        if self.live_count() >= self.max_live {
            return Err(BridgeError::Budget("live engines"));
        }
        if self
            .engines
            .values()
            .any(|v| v.state != EngineState::Unloaded && v.process.pid == process.pid)
        {
            return Err(BridgeError::Invalid("shared restored browser process"));
        }
        let v = self
            .engines
            .get_mut(&target)
            .ok_or(BridgeError::UnknownView)?;
        if v.state != EngineState::Unloaded {
            return Err(BridgeError::ReconciliationRequired);
        }
        if operation_id == 0 || operation_id <= v.operation_id {
            return Err(BridgeError::Duplicate);
        }
        if engine_incarnation <= v.engine_incarnation || process == v.process {
            return Err(BridgeError::Stale("fresh restored engine"));
        }
        if changed_input_or_cause.is_empty() || changed_input_or_cause.len() > 512 {
            return Err(BridgeError::Invalid("restore evidence/input basis"));
        }
        if v.restore_attempts >= 3 {
            return Err(BridgeError::Budget("restore attempts"));
        }
        if v.last_retry_basis.as_deref() == Some(changed_input_or_cause) {
            return Err(BridgeError::Invalid("unchanged restore retry basis"));
        }
        v.restore_attempts += 1;
        v.last_retry_basis = Some(changed_input_or_cause.to_owned());
        v.operation_id = operation_id;
        v.requested_ms = now_ms;
        v.engine_incarnation = engine_incarnation;
        v.process = process;
        v.state = EngineState::Restoring;
        Ok(RestorationStrength::PageReload)
    }
    /// Call only after independently linked real page pixels are accepted by the host.
    pub fn complete_restore(&mut self, target: ScopeTarget, operation_id: u64) -> Result<()> {
        let v = self
            .engines
            .get_mut(&target)
            .ok_or(BridgeError::UnknownView)?;
        if v.state != EngineState::Restoring || v.operation_id != operation_id {
            return Err(BridgeError::Stale("restore completion"));
        }
        v.state = if v.visible {
            EngineState::Visible
        } else {
            EngineState::Hidden
        };
        v.restore_attempts = 0;
        v.last_retry_basis = None;
        Ok(())
    }
    pub fn request_freeze(&self, target: ScopeTarget) -> Result<()> {
        self.record(target)?;
        Err(BridgeError::Unsupported(
            "screenshot bridge cannot freeze engine",
        ))
    }
    pub fn request_throttle(&self, target: ScopeTarget) -> Result<()> {
        self.record(target)?;
        Err(BridgeError::Unsupported(
            "screenshot bridge cannot throttle engine",
        ))
    }
    pub fn request_context_popout(&self, target: ScopeTarget) -> Result<()> {
        self.record(target)?;
        Err(BridgeError::Unsupported("engine/raw GPU context transfer"))
    }
}
