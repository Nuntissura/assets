//! Memory-only failure probes; these do not establish Win32/application compatibility.
use kumihimo_external_apps::{hosting::*, *};
use std::{cell::RefCell,rc::Rc};
#[derive(Clone)]
struct Fake(Rc<RefCell<State>>);
struct State { observed:NativeWindowSnapshot, epoch:u64, valid:bool, dispatched:Vec<HostingOperation>, completion:Option<Result<(),Diagnostic>> }
fn original()->NativeWindowSnapshot { NativeWindowSnapshot { parent:0,owner:0,style:7,ex_style:8,placement:Placement { bounds:PixelBounds { x:50,y:50,width:800,height:600 },state:WindowState::Normal },visible:true } }
fn allocation()->ChildAllocation { let bounds=PixelBounds { x:20,y:40,width:600,height:400 }; ChildAllocation { bounds,clip:bounds,parent_epoch:1 } }
impl HostingBackend for Fake {
    fn preflight(&mut self)->Result<HostingPreflight,Diagnostic> { Ok(HostingPreflight { original:original(),parent:999,parent_epoch:1,child_style:100,child_ex_style:8,minimum_width:100,minimum_height:100,limitations:vec!["Fixture only".into()] }) }
    fn validate(&mut self, epoch:u64)->Result<(),Diagnostic> { let state=self.0.borrow(); if !state.valid || state.epoch != epoch { Err(Diagnostic::new("stale","App/parent lifetime or epoch invalid")) } else { Ok(()) } }
    fn observe(&mut self)->Result<NativeWindowSnapshot,Diagnostic> { Ok(self.0.borrow().observed) }
    fn dispatch(&mut self, operation:HostingOperation)->Result<(),Diagnostic> { self.0.borrow_mut().dispatched.push(operation); Ok(()) }
    fn completion(&mut self)->Result<Option<Result<(),Diagnostic>>,Diagnostic> { Ok(self.0.borrow_mut().completion.take()) }
    fn current_parent_epoch(&self)->u64 { self.0.borrow().epoch }
    fn admission_key(&self)->String { format!("fixture:{}",Rc::as_ptr(&self.0) as usize) }
}
fn session()->(HostingSession<Fake>,Rc<RefCell<State>>) { let state=Rc::new(RefCell::new(State { observed:original(),epoch:1,valid:true,dispatched:vec![],completion:None })); (HostingSession::new(Fake(state.clone()),"fixture-app").unwrap(),state) }
fn attached(state:&Rc<RefCell<State>>) { let mut state=state.borrow_mut(); state.observed.parent=999;state.observed.style=100;state.observed.placement.bounds=allocation().bounds;state.completion=Some(Ok(())); }
#[test]
fn pending_worker_prevents_false_detach_and_conflicting_dispatch() {
    let (mut session,state)=session(); let result=session.request(HostingOperation::Attach(allocation()));
    assert_eq!(result.phase,HostingPhase::Unknown); assert!(result.pending);
    assert!(!session.before_parent_destroy().parent_destroy_allowed);
    session.request(HostingOperation::Detach); assert_eq!(state.borrow().dispatched.len(),1);
    attached(&state); assert_eq!(session.reconcile().phase,HostingPhase::Attached);
    assert!(!session.before_parent_destroy().parent_destroy_allowed);
    assert_eq!(state.borrow().dispatched.len(),2);
    { let mut state=state.borrow_mut();state.observed=original();state.completion=Some(Ok(())); }
    assert_eq!(session.reconcile().phase,HostingPhase::Detached);
    assert!(session.before_parent_destroy().parent_destroy_allowed);
}
#[test]
fn partial_attach_preserves_recovery_until_explicit_detach_is_observed() {
    let (mut session,state)=session(); session.request(HostingOperation::Attach(allocation()));
    { let mut state=state.borrow_mut();state.observed.style=100;state.completion=Some(Err(Diagnostic::new("set_parent_failed","Styles changed before parent rejection"))); }
    let result=session.reconcile(); assert_eq!(result.phase,HostingPhase::Failed);assert_eq!(result.diagnostic.unwrap().code,"set_parent_failed");assert!(!result.parent_destroy_allowed);
    let record=session.recovery();let bytes=serde_json::to_string(&record).unwrap();assert!(record.pending);assert!(!bytes.contains("hwnd"));assert!(!bytes.contains("pid"));
    assert!(!session.before_parent_destroy().parent_destroy_allowed);
    { let mut state=state.borrow_mut();state.observed=original();state.completion=Some(Ok(())); }
    assert!(session.reconcile().parent_destroy_allowed);
}
#[test]
fn stale_epoch_and_lifetime_or_partial_clips_reject_before_effect() {
    let (mut session,state)=session();let mut stale=allocation();stale.parent_epoch=2;
    assert!(session.request(HostingOperation::Attach(stale)).diagnostic.is_some());assert!(state.borrow().dispatched.is_empty());
    state.borrow_mut().valid=false;assert!(session.request(HostingOperation::Attach(allocation())).diagnostic.is_some());assert!(state.borrow().dispatched.is_empty());
    state.borrow_mut().valid=true;let mut clipped=allocation();clipped.clip.width-=1;
    assert!(session.request(HostingOperation::Attach(clipped)).diagnostic.is_some());assert!(state.borrow().dispatched.is_empty());
}
#[test]
fn observation_without_worker_completion_cannot_certify_attach() {
    let (mut session,state)=session();session.request(HostingOperation::Attach(allocation()));attached(&state);state.borrow_mut().completion=None;
    assert_eq!(session.reconcile().phase,HostingPhase::Unknown);assert!(!session.before_parent_destroy().parent_destroy_allowed);
    state.borrow_mut().completion=Some(Ok(()));assert_eq!(session.reconcile().phase,HostingPhase::Attached);
    session.request(HostingOperation::Detach);{ let mut state=state.borrow_mut();state.observed=original();state.completion=Some(Ok(())); }session.reconcile();
}
#[test]
fn completed_old_epoch_never_commits_current_attachment() {
    let (mut session,state)=session();session.request(HostingOperation::Attach(allocation()));attached(&state);state.borrow_mut().epoch=2;
    let receipt=session.reconcile();assert_eq!(receipt.phase,HostingPhase::Unknown);assert_eq!(receipt.diagnostic.unwrap().code,"stale_completed_epoch");assert!(!receipt.parent_destroy_allowed);
    session.request(HostingOperation::Detach);{let mut state=state.borrow_mut();state.observed=original();state.completion=Some(Ok(()));}assert!(session.reconcile().parent_destroy_allowed);
}
#[test]
fn unresolved_drop_is_bounded_inspectable_and_retrievable() {
    use kumihimo_external_apps::retention::{RecoveryBank,RECOVERY_CAPACITY};
    let bank=RecoveryBank::current_thread();let mut retained=vec![];
    for _ in 0..RECOVERY_CAPACITY {
        let (mut session,state)=session();let id=session.recovery_id();session.request(HostingOperation::Attach(allocation()));drop(session);retained.push((id,state));
    }
    assert_eq!(bank.inspect().iter().filter(|entry|entry.retrievable).count(),RECOVERY_CAPACITY);
    assert_eq!(RecoveryBank::current_thread().reserve("extra").err().unwrap().code,"recovery_capacity");
    for (id,state) in retained {
        let mut session=HostingSession::<Fake>::retrieve(bank.clone(),id).unwrap();
        attached(&state);session.reconcile();session.request(HostingOperation::Detach);
        {let mut state=state.borrow_mut();state.observed=original();state.completion=Some(Ok(()));}assert!(session.reconcile().parent_destroy_allowed);drop(session);
    }
    assert!(bank.inspect().is_empty());
}
#[test]
fn failed_automatic_detach_requires_explicit_retry_not_worker_wake_or_drop() {
    let (mut session,state)=session();session.request(HostingOperation::Attach(allocation()));attached(&state);session.reconcile();
    assert!(!session.before_parent_destroy().parent_destroy_allowed);assert_eq!(state.borrow().dispatched.len(),2);
    state.borrow_mut().completion=Some(Err(Diagnostic::new("detach_failed","Owned worker completed without restoration")));
    assert!(!session.before_parent_destroy().parent_destroy_allowed);
    for _ in 0..10 {assert!(!session.before_parent_destroy().parent_destroy_allowed);session.reconcile();}
    assert_eq!(state.borrow().dispatched.len(),2);
    let id=session.recovery_id();drop(session);assert_eq!(state.borrow().dispatched.len(),2);
    let mut session=HostingSession::<Fake>::retrieve(kumihimo_external_apps::retention::RecoveryBank::current_thread(),id).unwrap();
    session.request(HostingOperation::Detach);assert_eq!(state.borrow().dispatched.len(),3);
    {let mut state=state.borrow_mut();state.observed=original();state.completion=Some(Ok(()));}assert!(session.reconcile().parent_destroy_allowed);
}
