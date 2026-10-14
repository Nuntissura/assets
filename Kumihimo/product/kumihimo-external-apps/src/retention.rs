//! Explicit, bounded recovery ownership on the originating UI thread. Active and
//! retained child/runner objects share eight admission slots; effects reserve first.
use crate::Diagnostic;
use serde::Serialize;
use std::{any::Any,cell::RefCell,collections::BTreeMap,rc::Rc};
pub const RECOVERY_CAPACITY:usize=8;
struct Entry { status:String,key:Option<String>,diagnostic:Option<Diagnostic>,payload:Option<Box<dyn Any>> }
struct Bank { next:u64,capacity:usize,entries:BTreeMap<u64,Entry> }
impl Drop for Bank {
    fn drop(&mut self) {
        // Origin-thread/process teardown cannot safely destroy attached native parents.
        // This is bounded by the same eight slots, never silent cleanup success.
        for (id,entry) in &mut self.entries {
            if let Some(payload)=entry.payload.take() {
                eprintln!("{{\"event\":\"native_recovery_thread_exit_unresolved\",\"recovery_id\":{id}}}");
                std::mem::forget(payload);
            }
        }
    }
}
thread_local! { static CURRENT:Rc<RefCell<Bank>>=Rc::new(RefCell::new(Bank { next:1,capacity:RECOVERY_CAPACITY,entries:BTreeMap::new() })); }
#[derive(Clone)]
pub struct RecoveryBank(Rc<RefCell<Bank>>);
#[derive(Clone,Debug,Serialize)]
pub struct RecoveryEntry { pub id:u64,pub status:String,pub diagnostic:Option<Diagnostic>,pub retrievable:bool }
pub struct RecoveryAdmission { bank:RecoveryBank,id:u64 }
impl RecoveryBank {
    /// The singleton is explicit and inspectable; new banks cannot bypass this thread's cap.
    pub fn current_thread()->Self { CURRENT.with(|bank|Self(bank.clone())) }
    /// Deliberate host resource policy, independent of the conceptual pane count.
    pub fn configure_capacity(&self,capacity:usize)->Result<(),Diagnostic> {
        let mut bank=self.0.borrow_mut();
        if capacity==0 || capacity>64 || !bank.entries.is_empty() { return Err(Diagnostic::new("recovery_capacity_change", "Capacity is 1..64 and may change only before active/retained admissions")); }
        bank.capacity=capacity;Ok(())
    }
    pub fn inspect(&self)->Vec<RecoveryEntry> { self.0.borrow().entries.iter().map(|(id,entry)|RecoveryEntry { id:*id,status:entry.status.clone(),diagnostic:entry.diagnostic.clone(),retrievable:entry.payload.is_some() }).collect() }
    pub fn reserve(&self,scope:&str)->Result<RecoveryAdmission,Diagnostic> {
        self.reserve_key(scope,None)
    }
    pub fn reserve_key(&self,scope:&str,key:Option<&str>)->Result<RecoveryAdmission,Diagnostic> {
        if scope.len()>512 { return Err(Diagnostic::new("recovery_scope_size", "Recovery scope exceeds 512 bytes")); }
        let mut bank=self.0.borrow_mut();
        if key.is_some_and(|key|bank.entries.values().any(|entry|entry.key.as_deref()==Some(key))) { return Err(Diagnostic::new("recovery_conflict", "This exact app/runner has an active or retained recovery owner")); }
        if bank.entries.len()>=bank.capacity { return Err(Diagnostic::new("recovery_capacity", "Active/retained recovery capacity is exhausted; retrieve and reconcile an owner before new effects")); }
        let id=bank.next;bank.next=bank.next.checked_add(1).ok_or_else(||Diagnostic::new("recovery_id_exhausted", "Recovery IDs exhausted"))?;
        bank.entries.insert(id,Entry { status:scope.into(),key:key.map(str::to_owned),diagnostic:None,payload:None });Ok(RecoveryAdmission { bank:self.clone(),id })
    }
    pub fn retrieve<T:'static>(&self,id:u64)->Result<(T,RecoveryAdmission),Diagnostic> {
        let mut bank=self.0.borrow_mut();let entry=bank.entries.get_mut(&id).ok_or_else(||Diagnostic::new("recovery_missing", "Recovery ID is absent"))?;
        if !entry.payload.as_ref().is_some_and(|payload|payload.is::<T>()) { return Err(Diagnostic::new("recovery_type", "Retained owner is active or belongs to a different recovery API")); }
        let payload=entry.payload.take().unwrap().downcast::<T>().map_err(|_|Diagnostic::new("recovery_type", "Retained owner type differs"))?;
        entry.status="retrieved_for_reconciliation".into();Ok((*payload,RecoveryAdmission { bank:self.clone(),id }))
    }
}
impl RecoveryAdmission {
    pub fn id(&self)->u64 { self.id }
    pub fn bank(&self)->RecoveryBank { self.bank.clone() }
    pub fn retain<T:'static>(&self,payload:T,diagnostic:Diagnostic) {
        let mut bank=self.bank.0.borrow_mut();let entry=bank.entries.get_mut(&self.id).expect("live admission is reserved");
        assert!(entry.payload.is_none(),"one retained payload per admission");
        entry.status="unresolved_recovery_owner".into();entry.diagnostic=Some(diagnostic);entry.payload=Some(Box::new(payload));
        eprintln!("{{\"event\":\"native_recovery_retained\",\"recovery_id\":{},\"retrievable\":true}}",self.id);
    }
}
impl Drop for RecoveryAdmission {
    fn drop(&mut self) { let mut bank=self.bank.0.borrow_mut();if bank.entries.get(&self.id).is_some_and(|entry|entry.payload.is_none()) {bank.entries.remove(&self.id);} }
}
