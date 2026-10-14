use kumihimo_core::*;
use kumihimo_external_apps::*;
use std::{cell::RefCell, rc::Rc};

fn bounds(x: i32, width: i32) -> PixelBounds { PixelBounds { x, y: 20, width, height: 200 } }
fn placement(x: i32, width: i32) -> Placement { Placement { bounds: bounds(x, width), state: WindowState::Normal } }
fn identity() -> WindowIdentity { WindowIdentity { hwnd: 11, pid: 22, creation_time: 33 } }
struct State { current: Placement, identity: WindowIdentity, lifetime: u64, requests: usize, partial: bool, reject_restore: bool, replace_after_request: bool, delay: bool, delayed: Option<PixelBounds> }
struct Fake(Rc<RefCell<State>>);
impl WindowBackend for Fake {
    type Lease = (WindowIdentity, u64);
    fn bind(&mut self, target: WindowTarget) -> Result<(Self::Lease, WindowIdentity), Diagnostic> {
        let id = self.0.borrow().identity;
        if target.hwnd != id.hwnd || target.pid != id.pid || target.expected_creation_time.is_some_and(|value| value != id.creation_time) { return Err(Diagnostic::new("mismatch", "Selection mismatch")); }
        Ok(((id, self.0.borrow().lifetime), id))
    }
    fn validate(&mut self, lease: &Self::Lease, identity: WindowIdentity) -> Result<(), Diagnostic> {
        if lease.0 != identity || self.0.borrow().identity != identity || self.0.borrow().lifetime != lease.1 { Err(Diagnostic::new("replaced", "Identity or window-lifetime witness changed")) } else { Ok(()) }
    }
    fn observe(&mut self, _: &Self::Lease) -> Result<Placement, Diagnostic> { Ok(self.0.borrow().current) }
    fn capabilities(&mut self, _: &Self::Lease) -> Result<Capabilities, Diagnostic> { Ok(Capabilities { work_area: PixelBounds { x:0,y:0,width:1920,height:1080 }, dpi:96, minimum_width:100,minimum_height:100,can_resize:true }) }
    fn request_bounds(&mut self, _: &Self::Lease, bounds: PixelBounds) -> Result<(), Diagnostic> {
        let mut state = self.0.borrow_mut(); state.requests += 1;
        if state.delay { state.delayed = Some(bounds); return Ok(()); }
        state.current.bounds = bounds;
        if state.partial { state.current.bounds.width -= 10; }
        if state.replace_after_request { state.identity.creation_time += 1; }
        if state.partial { Err(Diagnostic::new("partial_os_error", "Request changed bounds before error")) } else { Ok(()) }
    }
    fn request_completed(&mut self, _: &Self::Lease) -> Result<bool, Diagnostic> { Ok(!self.0.borrow().delay) }
    fn request_restore(&mut self, _: &Self::Lease, placement: Placement) -> Result<(), Diagnostic> {
        let mut state = self.0.borrow_mut(); state.requests += 1;
        if state.reject_restore { return Err(Diagnostic::new("restore_failed", "Restore rejected")); }
        state.current = placement; Ok(())
    }
    fn observation_pause(&mut self, _: u64) {}
}
fn host() -> (ExternalApps<Fake>, SelectedWindow<(WindowIdentity,u64)>, Rc<RefCell<State>>) {
    let state = Rc::new(RefCell::new(State { current:placement(30,300),identity:identity(),lifetime:1,requests:0,partial:false,reject_restore:false,replace_after_request:false,delay:false,delayed:None }));
    let mut host = ExternalApps::new(Fake(state.clone()));
    let selected = host.bind(WindowTarget { hwnd:11,pid:22,expected_creation_time:Some(33) }, "owned-app").unwrap();
    (host, selected, state)
}
fn projection() -> (GeometryOutput, FrameId) {
    let mut core = Core::new(Identity::new("external-test", "test").unwrap(), Limits::default(), 8).unwrap();
    let mut apply = |command| core.execute(Envelope { identity:core.snapshot().identity.clone(),actor:"test".into(),operation:core.ledger_status().high_watermark.checked_next().unwrap(),expected_generation:core.snapshot().generation,command }).outcome.result.unwrap();
    let Change::Created(EntityId::Workspace(ws)) = apply(Command::CreateWorkspace { owner:None }) else { panic!() };
    let Change::Created(EntityId::Frame(frame)) = apply(Command::CreateFrame { workspace:ws,index:0 }) else { panic!() };
    apply(Command::SetGeometryContext { workspace:ws, context:GeometryContext::new(Rect::new(50.0,20.0,400.0,200.0).unwrap()) });
    (core.snapshot().geometry(ws).unwrap(), frame)
}
fn tile(host: &mut ExternalApps<Fake>, selected: &SelectedWindow<(WindowIdentity,u64)>) -> EffectReceipt {
    let (geometry, frame) = projection();
    host.tile(selected, &geometry, frame, DesktopMapping { origin_x:0,origin_y:0,pixels_per_dip:1.0 }, ObserveBudget { attempts:2,interval_ms:0 })
}
#[test]
fn reused_window_or_process_is_rejected_before_effect() {
    for replacement in [WindowIdentity { hwnd:99,..identity() }, WindowIdentity { pid:99,..identity() }, WindowIdentity { creation_time:99,..identity() }] {
        let (mut host, selected, state) = host(); state.borrow_mut().identity = replacement;
        assert_eq!(tile(&mut host, &selected).outcome, EffectOutcome::Rejected);
        assert_eq!(host.restore(&selected, ObserveBudget::default()).outcome, EffectOutcome::Rejected);
        assert_eq!(state.borrow().requests, 0);
    }
    let (mut host, selected, state) = host();
    state.borrow_mut().lifetime += 1; // HWND, PID, process start and owner thread all unchanged.
    assert_eq!(tile(&mut host, &selected).outcome, EffectOutcome::Rejected);
    assert_eq!(state.borrow().requests, 0);
}
#[test]
fn delayed_request_prevents_false_detach_until_its_effect_is_observed() {
    let (mut host, selected, state) = host(); state.borrow_mut().delay = true;
    let receipt = tile(&mut host, &selected);
    assert_eq!(receipt.outcome, EffectOutcome::Unknown); assert!(receipt.pending.is_some());
    let detached = host.restore(&selected, ObserveBudget { attempts:2,interval_ms:0 });
    assert_eq!(detached.outcome, EffectOutcome::Unknown); assert!(detached.pending.is_some());
    assert_eq!(state.borrow().requests, 1); assert_eq!(state.borrow().current, selected.original());
    assert!(RecoveryRecord::from_selection(&selected).pending.is_some());
    { let mut state = state.borrow_mut(); state.current.bounds = state.delayed.take().unwrap(); }
    let detached = host.restore(&selected, ObserveBudget { attempts:2,interval_ms:0 });
    assert_eq!(detached.outcome, EffectOutcome::Applied); assert!(detached.pending.is_none());
    assert_eq!(state.borrow().requests, 2); assert_eq!(state.borrow().current, selected.original());
}
#[test]
fn recovery_cannot_discard_a_live_pending_dispatch_and_non_normal_bind_rejects() {
    let (mut host, mut selected, state) = host();
    let prior = RecoveryRecord::from_selection(&selected);
    state.borrow_mut().delay = true; tile(&mut host, &selected);
    assert_eq!(prior.apply_to(&mut selected).unwrap_err().code, "pending_recovery_conflict");
    assert!(selected.pending().is_some());
    for state_kind in [WindowState::Minimized, WindowState::Maximized] {
        state.borrow_mut().current.state = state_kind;
        assert!(host.bind(WindowTarget { hwnd:11,pid:22,expected_creation_time:Some(33) }, "another-explicit-selection").is_err());
    }
}
#[test]
fn partial_os_effect_preserves_error_and_compensates_once() {
    let (mut host, selected, state) = host(); state.borrow_mut().partial = true;
    let receipt = tile(&mut host, &selected);
    assert_eq!(receipt.outcome, EffectOutcome::Partial);
    assert_eq!(receipt.request_error.unwrap().code, "partial_os_error");
    assert_eq!(receipt.observed.unwrap().bounds.width, 390);
    assert_eq!(receipt.compensation.unwrap().outcome, EffectOutcome::Applied);
    assert_eq!(state.borrow().current, selected.original()); assert_eq!(state.borrow().requests, 2);
}
#[test]
fn failed_compensation_remains_visible_and_recovery_can_retry() {
    let (mut host, selected, state) = host(); { let mut state = state.borrow_mut(); state.partial = true; state.reject_restore = true; }
    let receipt = tile(&mut host, &selected);
    let compensation = receipt.compensation.unwrap();
    assert_eq!(receipt.outcome, EffectOutcome::Partial); assert_eq!(compensation.outcome, EffectOutcome::Failed);
    assert_eq!(compensation.request_error.unwrap().code, "restore_failed"); assert_eq!(state.borrow().requests, 2);
    state.borrow_mut().reject_restore = false;
    assert_eq!(host.restore(&selected, ObserveBudget::default()).outcome, EffectOutcome::Applied);
    assert_eq!(state.borrow().current, selected.original());
}
#[test]
fn replacement_after_dispatch_is_unknown_and_never_compensates_new_identity() {
    let (mut host, selected, state) = host(); state.borrow_mut().replace_after_request = true;
    let receipt = tile(&mut host, &selected);
    assert_eq!(receipt.outcome, EffectOutcome::Unknown); assert!(receipt.observation_error.is_some()); assert!(receipt.compensation.is_none()); assert_eq!(state.borrow().requests, 1);
}
#[test]
fn recovery_is_semantic_and_rejects_wrong_reference_or_schema() {
    let (_, mut selected, _) = host(); let mut record = RecoveryRecord::from_selection(&selected);
    let encoded = serde_json::to_string(&record).unwrap();
    for forbidden in ["hwnd", "pid", "creation_time"] { assert!(!encoded.contains(forbidden)); }
    record.schema = 2; assert!(record.apply_to(&mut selected).is_err()); record.schema = 1;
    record.semantic_reference = "other".into(); assert!(record.apply_to(&mut selected).is_err());
    record.semantic_reference = "owned-app".into(); record.apply_to(&mut selected).unwrap();
}
