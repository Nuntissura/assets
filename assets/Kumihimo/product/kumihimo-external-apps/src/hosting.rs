//! Explicit native-child lifecycle. One pending operation per binding; no implicit retry.
use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum HostingPhase { Preflight, Attaching, Attached, Detaching, Detached, Failed, Unknown }
/// Live native observations. Native parent/owner handles are never persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeWindowSnapshot {
    pub parent: u64,
    pub owner: u64,
    pub style: u32,
    pub ex_style: u32,
    pub placement: Placement,
    pub visible: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostingRecovery {
    pub schema: u32,
    pub semantic_reference: String,
    pub original_style: u32,
    pub original_ex_style: u32,
    pub original_placement: Placement,
    pub original_visible: bool,
    /// Retained semantic obligation; never permission to replay against a discovered window.
    pub pending: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChildAllocation { pub bounds: PixelBounds, pub clip: PixelBounds, pub parent_epoch: u64 }
impl ChildAllocation {
    pub fn validate(self) -> Result<(), Diagnostic> {
        self.bounds.validate()?; self.clip.validate()?;
        if self.parent_epoch == 0 || self.bounds != self.clip {
            Err(Diagnostic::new("unsupported_child_clip", "Native-child route requires current epoch and a fully visible, unobscured allocation"))
        } else { Ok(()) }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostingOperation {
    Attach(ChildAllocation), Resize(ChildAllocation), Visibility(bool),
    /// Only an explicit user gesture may request focus; backend rechecks OS foreground.
    Focus { explicit_user_action: bool }, Detach,
}
#[derive(Clone, Debug)]
pub struct HostingPreflight {
    pub original: NativeWindowSnapshot,
    pub parent: u64,
    pub parent_epoch: u64,
    pub child_style: u32,
    pub child_ex_style: u32,
    pub minimum_width: i32,
    pub minimum_height: i32,
    pub limitations: Vec<String>,
}
/// All mutation dispatch is bounded/nonblocking. Completion means the worker has
/// finished every synchronous OS call; it does not by itself prove the requested state.
pub trait HostingBackend {
    fn preflight(&mut self) -> Result<HostingPreflight, Diagnostic>;
    fn validate(&mut self, parent_epoch: u64) -> Result<(), Diagnostic>;
    fn observe(&mut self) -> Result<NativeWindowSnapshot, Diagnostic>;
    fn dispatch(&mut self, operation: HostingOperation) -> Result<(), Diagnostic>;
    fn completion(&mut self) -> Result<Option<Result<(), Diagnostic>>, Diagnostic>;
    fn current_parent_epoch(&self) -> u64;
    fn admission_key(&self)->String;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostingReceipt {
    pub semantic_reference: String,
    pub phase: HostingPhase,
    pub pending: bool,
    pub parent_destroy_allowed: bool,
    pub diagnostic: Option<Diagnostic>,
    pub limitations: Vec<String>,
}

pub struct HostingSession<B: HostingBackend + 'static> {
    backend: std::mem::ManuallyDrop<B>,
    semantic_reference: String,
    preflight: HostingPreflight,
    phase: HostingPhase,
    pending: Option<HostingOperation>,
    completed: Option<Result<(), Diagnostic>>,
    allocation: Option<ChildAllocation>,
    diagnostic: Option<Diagnostic>,
    parent_destroy_allowed: bool,
    admission: crate::retention::RecoveryAdmission,
    detach_attempted:bool,
}
struct RetainedHosting<B:HostingBackend+'static> { backend:B,semantic_reference:String,preflight:HostingPreflight,phase:HostingPhase,pending:Option<HostingOperation>,completed:Option<Result<(),Diagnostic>>,allocation:Option<ChildAllocation>,diagnostic:Option<Diagnostic>,parent_destroy_allowed:bool,detach_attempted:bool }
impl<B: HostingBackend+'static> HostingSession<B> {
    pub fn new(backend: B, semantic_reference: &str) -> Result<Self, Diagnostic> {
        Self::new_in(backend,semantic_reference,crate::retention::RecoveryBank::current_thread())
    }
    pub fn new_in(mut backend:B,semantic_reference:&str,bank:crate::retention::RecoveryBank)->Result<Self,Diagnostic> {
        if semantic_reference.is_empty() || semantic_reference.len() > 512 { return Err(Diagnostic::new("invalid_reference", "Hosting reference requires 1..512 bytes")); }
        let key=backend.admission_key();
        let admission=bank.reserve_key(semantic_reference,Some(&key))?;
        let preflight = backend.preflight()?;
        if preflight.original.parent != 0 || preflight.original.owner != 0 || preflight.original.placement.state != WindowState::Normal {
            return Err(Diagnostic::new("unsupported_original_parent", "Only an unowned normal desktop window has a supported durable fallback"));
        }
        preflight.original.placement.bounds.validate()?;
        Ok(Self { backend:std::mem::ManuallyDrop::new(backend), semantic_reference:semantic_reference.into(), preflight, phase:HostingPhase::Preflight, pending:None, completed:None, allocation:None, diagnostic:None, parent_destroy_allowed:true,admission,detach_attempted:false })
    }
    pub fn recovery_id(&self)->u64 { self.admission.id() }
    pub fn retrieve(bank:crate::retention::RecoveryBank,id:u64)->Result<Self,Diagnostic> {
        let (held,admission)=bank.retrieve::<RetainedHosting<B>>(id)?;
        Ok(Self { backend:std::mem::ManuallyDrop::new(held.backend),semantic_reference:held.semantic_reference,preflight:held.preflight,phase:held.phase,pending:held.pending,completed:held.completed,allocation:held.allocation,diagnostic:held.diagnostic,parent_destroy_allowed:held.parent_destroy_allowed,admission,detach_attempted:held.detach_attempted })
    }
    pub fn phase(&self) -> HostingPhase { self.phase }
    pub fn parent_handle(&self)->u64 {self.preflight.parent}
    /// No queued effect may later make a child cover a parent overlay.
    pub fn presentation_clear_for_overlay(&mut self)->Result<bool,Diagnostic> {
        self.reconcile();if self.pending(){return Ok(false);}
        let epoch=self.backend.current_parent_epoch();self.backend.validate(epoch)?;
        let observed=self.backend.observe()?;
        Ok(observed.parent==0 || (observed.parent==self.preflight.parent && !observed.visible))
    }
    pub fn pending(&self) -> bool { self.pending.is_some() }
    pub fn backend_mut(&mut self) -> &mut B { &mut self.backend }
    pub fn receipt(&self) -> HostingReceipt {
        HostingReceipt { semantic_reference:self.semantic_reference.clone(), phase:self.phase, pending:self.pending(), parent_destroy_allowed:self.parent_destroy_allowed, diagnostic:self.diagnostic.clone(), limitations:self.preflight.limitations.clone() }
    }
    pub fn recovery(&self) -> HostingRecovery {
        let original = self.preflight.original;
        HostingRecovery { schema:1, semantic_reference:self.semantic_reference.clone(), original_style:original.style, original_ex_style:original.ex_style, original_placement:original.placement, original_visible:original.visible, pending:self.pending() || !self.parent_destroy_allowed }
    }
    fn reject(&mut self, diagnostic: Diagnostic) -> HostingReceipt { self.diagnostic = Some(diagnostic); self.receipt() }
    pub fn request(&mut self, operation: HostingOperation) -> HostingReceipt {
        self.reconcile();
        if self.pending() { return self.reject(Diagnostic::new("hosting_pending", "Exact prior worker must complete and be observed before another effect")); }
        let epoch = match operation {
            HostingOperation::Attach(allocation) | HostingOperation::Resize(allocation) => {
                if let Err(error) = allocation.validate() { return self.reject(error); }
                if allocation.bounds.width < self.preflight.minimum_width || allocation.bounds.height < self.preflight.minimum_height { return self.reject(Diagnostic::new("unsupported_child_size", "Allocation is below the independently queried application minimum")); }
                allocation.parent_epoch
            },
            HostingOperation::Detach => self.backend.current_parent_epoch(),
            _ => self.allocation.map_or(self.preflight.parent_epoch, |allocation| allocation.parent_epoch),
        };
        if let Err(error) = self.backend.validate(epoch) { self.phase = HostingPhase::Unknown; self.parent_destroy_allowed = false; return self.reject(error); }
        match operation {
            HostingOperation::Attach(_) if !matches!(self.phase, HostingPhase::Preflight | HostingPhase::Detached) => return self.reject(Diagnostic::new("hosting_state", "Attach requires preflight or detached state")),
            HostingOperation::Resize(_) | HostingOperation::Visibility(_) | HostingOperation::Focus { .. } if self.phase != HostingPhase::Attached => return self.reject(Diagnostic::new("hosting_state", "Resize/visibility/focus requires attached state")),
            HostingOperation::Focus { explicit_user_action:false } => return self.reject(Diagnostic::new("focus_not_authorized", "No explicit user focus action supplied")),
            _ => {},
        }
        if let HostingOperation::Attach(allocation) | HostingOperation::Resize(allocation) = operation { self.allocation = Some(allocation); }
        if operation==HostingOperation::Detach {self.detach_attempted=true;} else if matches!(operation,HostingOperation::Attach(_)) {self.detach_attempted=false;}
        self.phase = if operation == HostingOperation::Detach { HostingPhase::Detaching } else if matches!(operation, HostingOperation::Attach(_)) { HostingPhase::Attaching } else { self.phase };
        self.parent_destroy_allowed = false;
        self.pending = Some(operation); self.completed = None; self.diagnostic = None;
        if let Err(error) = self.backend.dispatch(operation) {
            self.pending = None; self.phase = HostingPhase::Failed; self.diagnostic = Some(error);
        }
        self.reconcile()
    }
    /// A single nonblocking completion/read pass. Wake-driven callers do not poll/spin.
    pub fn reconcile(&mut self) -> HostingReceipt {
        let Some(operation) = self.pending else { return self.receipt(); };
        if self.completed.is_none() {
            match self.backend.completion() {
                Ok(Some(completed)) => self.completed = Some(completed),
                Ok(None) => { self.phase = HostingPhase::Unknown; return self.receipt(); },
                Err(error) => { self.phase = HostingPhase::Unknown; return self.reject(error); },
            }
        }
        let epoch = match operation { HostingOperation::Attach(allocation)|HostingOperation::Resize(allocation)=>allocation.parent_epoch,_=>self.backend.current_parent_epoch() };
        if self.backend.current_parent_epoch()!=epoch {
            // Completion settles the worker, but never makes old geometry current.
            self.pending=None;self.completed=None;self.phase=HostingPhase::Unknown;self.parent_destroy_allowed=false;
            return self.reject(Diagnostic::new("stale_completed_epoch", "Worker completed against a superseded parent epoch; explicit current-epoch detach/recovery required"));
        }
        if let Err(error) = self.backend.validate(epoch) { self.phase = HostingPhase::Unknown; return self.reject(error); }
        let observed = match self.backend.observe() { Ok(observed) => observed, Err(error) => { self.phase = HostingPhase::Unknown; return self.reject(error); } };
        let expected = match operation {
            HostingOperation::Detach => observed == self.preflight.original,
            HostingOperation::Attach(allocation) | HostingOperation::Resize(allocation) => observed.parent == self.preflight.parent && observed.style == self.preflight.child_style && observed.ex_style == self.preflight.child_ex_style && observed.placement.bounds == allocation.bounds && observed.visible,
            HostingOperation::Visibility(visible) => observed.parent == self.preflight.parent && observed.visible == visible,
            // Native backend completion additionally independently checks OS focus ownership.
            HostingOperation::Focus { .. } => self.completed.as_ref().is_some_and(Result::is_ok) && observed.parent == self.preflight.parent,
        };
        let request_error = self.completed.take().and_then(Result::err);
        self.pending = None;
        if expected {
            self.phase = if operation == HostingOperation::Detach { HostingPhase::Detached } else { HostingPhase::Attached };
            self.parent_destroy_allowed = operation == HostingOperation::Detach;
            self.diagnostic = request_error;
        } else {
            self.phase = HostingPhase::Failed;
            self.diagnostic = request_error.or_else(|| Some(Diagnostic::new("hosting_partial_effect", "Completed worker state differs from requested native parent/styles/geometry; explicit detach compensation required")));
        }
        self.receipt()
    }
    /// Fresh canonical re-read, never a cached phase-only teardown verdict.
    pub fn before_parent_destroy(&mut self) -> HostingReceipt {
        self.reconcile();
        if self.pending() { self.parent_destroy_allowed = false; return self.receipt(); }
        let epoch = self.backend.current_parent_epoch();
        match self.backend.validate(epoch).and_then(|_| self.backend.observe()) {
            Ok(observed) if observed == self.preflight.original => { self.parent_destroy_allowed = true; },
            Ok(_) => { self.parent_destroy_allowed = false;if !self.detach_attempted {self.request(HostingOperation::Detach);} },
            Err(error) => { self.parent_destroy_allowed = false; self.phase = HostingPhase::Unknown; self.diagnostic = Some(error); },
        }
        self.receipt()
    }
}
impl<B: HostingBackend+'static> Drop for HostingSession<B> {
    fn drop(&mut self) {
        let receipt = self.before_parent_destroy();
        if receipt.parent_destroy_allowed {
            // Only a fresh observed desktop restoration permits release of native leases.
            unsafe { std::mem::ManuallyDrop::drop(&mut self.backend); }
        } else {
            let diagnostic=self.diagnostic.clone().unwrap_or_else(||Diagnostic::new("hosting_drop_unresolved", "Retrieve this recovery ID on the originating UI thread and reconcile before native parent/process teardown"));
            let held=RetainedHosting { backend:unsafe {std::mem::ManuallyDrop::take(&mut self.backend)},semantic_reference:std::mem::take(&mut self.semantic_reference),preflight:self.preflight.clone(),phase:self.phase,pending:self.pending,completed:self.completed.take(),allocation:self.allocation,diagnostic:self.diagnostic.clone(),parent_destroy_allowed:false,detach_attempted:self.detach_attempted };
            self.admission.retain(held,diagnostic);
        }
    }
}
