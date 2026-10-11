use buurtman_balie::{memory::MemoryStore, worker::{CommandWorker, WorkerConfig, WorkerPressure}, *};
use std::{collections::BTreeMap, sync::{Arc, Mutex, Barrier, mpsc, atomic::{AtomicBool, AtomicUsize, Ordering}}, thread, time::{Duration, Instant}};

fn config() -> WorkerConfig { WorkerConfig { pending_count:32, max_request_bytes:16_384, outstanding_bytes:524_288, queue_deadline:Duration::from_secs(3), caller_deadline:Duration::from_secs(3) } }
fn ctx() -> Context { Context { workspace:"business".into(),actor:"operator".into() } }
struct Grants(Arc<AtomicBool>);
impl Authorization for Grants { fn allows(&self,c:&Context,_:Access)->bool { self.0.load(Ordering::SeqCst) && c == &ctx() } }
fn grants()->Grants { Grants(Arc::new(AtomicBool::new(true))) }
fn record(id:&str)->Record { Record { id:id.into(),kind:Kind::Interaction,name:id.into(),description:String::new(),tags:vec!["priority".into()],fields:BTreeMap::new(),case_status:None,client_terms:None,prices:None,revision:0 } }
fn request(id:&str)->Request { Request {operation_id:format!("create-{id}"),command:Command::Create(record(id))} }
struct Block {entered:mpsc::Sender<()>,release:mpsc::Receiver<()>}
struct Gate {entered:mpsc::Receiver<()>,release:mpsc::Sender<()>}
fn gate()->(Block,Gate) { let (tx,rx)=mpsc::channel();let (a,b)=mpsc::channel();(Block{entered:tx,release:b},Gate{entered:rx,release:a}) }
impl Block {fn wait(self){self.entered.send(()).unwrap();self.release.recv_timeout(Duration::from_secs(5)).unwrap();}}
impl Gate {fn entered(&self){self.entered.recv_timeout(Duration::from_secs(5)).unwrap();} fn release(self){self.release.send(()).unwrap();}}
struct Controlled {canonical:Arc<Mutex<MemoryStore>>,load_gate:Mutex<Option<Block>>,after_load:Mutex<Option<Block>>,committed:Option<Block>,loads:Arc<AtomicUsize>,writes:Arc<AtomicUsize>,panic_load:bool}
impl Controlled {fn new()->Self {Self{canonical:Arc::new(Mutex::new(MemoryStore::new(2))),load_gate:Mutex::new(None),after_load:Mutex::new(None),committed:None,loads:Arc::new(AtomicUsize::new(0)),writes:Arc::new(AtomicUsize::new(0)),panic_load:false}}}
impl Store for Controlled {
 fn load(&self,w:&str)->Result<Snapshot,Error> {self.loads.fetch_add(1,Ordering::SeqCst);if let Some(b)=self.load_gate.lock().unwrap().take(){b.wait();}if self.panic_load{panic!("host load panic");}let value=self.canonical.lock().unwrap().load(w)?;if let Some(b)=self.after_load.lock().unwrap().take(){b.wait();}Ok(value)}
 fn compare_exchange(&mut self,w:&str,e:u64,n:Snapshot)->Result<(),Error>{self.writes.fetch_add(1,Ordering::SeqCst);self.canonical.lock().unwrap().compare_exchange(w,e,n)?;if let Some(b)=self.committed.take(){b.wait();}Ok(())}
}
fn observe(h:&CommandWorker,p:impl Fn(&WorkerPressure)->bool)->WorkerPressure {let end=Instant::now()+Duration::from_secs(5);loop {let v=h.pressure();if p(&v){return v;}assert!(Instant::now()<end,"pressure: {v:?}");thread::yield_now();}}
fn drained(h:&CommandWorker){h.close();let p=observe(h,|p|!p.worker_alive);assert_eq!((p.pending_count,p.running_count,p.ready_count,p.charged_bytes),(0,0,0,0));}

#[test]
fn simultaneous_unique_commands_keep_exact_receipts_and_stale_edits_conflict(){
 let store=Controlled::new();let canonical=store.canonical.clone();let writes=store.writes.clone();let h=CommandWorker::new(store,grants(),Limits::default(),config()).unwrap();let start=Arc::new(Barrier::new(17));let mut threads=Vec::new();
 for i in 0..16 {let handle=h.clone();let barrier=start.clone();threads.push(thread::spawn(move||{barrier.wait();handle.execute(&ctx(),request(&format!("r{i}")))}));}start.wait();for t in threads{assert!(t.join().unwrap().is_ok());}
 let s=canonical.lock().unwrap().load("business").unwrap();assert_eq!((s.records.len(),s.receipts.len(),s.revision),(16,16,16));for i in 0..16 {let r=request(&format!("r{i}"));assert_eq!(s.receipts[&r.operation_id].request,r);assert_eq!(s.receipts[&r.operation_id].actor,"operator");}assert_eq!(writes.load(Ordering::SeqCst),16);
 let mut edited=h.get(&ctx(),"r0").unwrap();edited.name="changed".into();let update=Request{operation_id:"edit".into(),command:Command::Update(edited.clone())};h.execute(&ctx(),update).unwrap();assert_eq!(h.execute(&ctx(),Request{operation_id:"stale".into(),command:Command::Update(edited)}),Err(Error::Conflict));assert_eq!(writes.load(Ordering::SeqCst),17);assert_eq!(h.get(&ctx(),"r0").unwrap().revision,2);drained(&h);
}

#[test]
fn external_writer_conflict_is_not_retried_or_overwritten(){
 let (block,gate)=gate();let store=Controlled::new();*store.after_load.lock().unwrap()=Some(block);let canonical=store.canonical.clone();let writes=store.writes.clone();let h=CommandWorker::new(store,grants(),Limits::default(),config()).unwrap();let a=h.clone();let active=thread::spawn(move||a.execute(&ctx(),request("local")));gate.entered();
 let mut external=Crm::new(Shared(canonical.clone()),grants(),Limits::default());external.execute(&ctx(),request("external")).unwrap();let before=canonical.lock().unwrap().load("business").unwrap();gate.release();assert_eq!(active.join().unwrap(),Err(Error::Conflict));assert_eq!(writes.load(Ordering::SeqCst),1);assert_eq!(canonical.lock().unwrap().load("business").unwrap(),before);drained(&h);
}
struct Shared(Arc<Mutex<MemoryStore>>);
impl Store for Shared {fn load(&self,w:&str)->Result<Snapshot,Error>{self.0.lock().unwrap().load(w)}fn compare_exchange(&mut self,w:&str,e:u64,n:Snapshot)->Result<(),Error>{self.0.lock().unwrap().compare_exchange(w,e,n)}}

#[test]
fn queued_revocation_checks_current_grants_before_store_access(){
 let (block,gate)=gate();let store=Controlled::new();*store.load_gate.lock().unwrap()=Some(block);let loads=store.loads.clone();let writes=store.writes.clone();let auth=grants();let grant=auth.0.clone();let h=CommandWorker::new(store,auth,Limits::default(),config()).unwrap();let a=h.clone();let active=thread::spawn(move||a.field_definitions(&ctx()));gate.entered();let b=h.clone();let queued=thread::spawn(move||b.execute(&ctx(),request("denied")));observe(&h,|p|p.pending_count==1);grant.store(false,Ordering::SeqCst);gate.release();active.join().unwrap().unwrap();assert_eq!(queued.join().unwrap(),Err(Error::Denied));assert_eq!(loads.load(Ordering::SeqCst),1);assert_eq!(writes.load(Ordering::SeqCst),0);drained(&h);
}

#[test]
fn exact_context_tuple_bytes_and_count_limits_reject_without_leaks(){
 let (block,gate)=gate();let store=Controlled::new();*store.load_gate.lock().unwrap()=Some(block);let loads=store.loads.clone();let mut policy=config();policy.pending_count=1;let expected=serde_json::to_vec(&(ctx(),"r")).unwrap().len();policy.max_request_bytes=expected;policy.outstanding_bytes=expected*2;let h=CommandWorker::new(store,grants(),Limits::default(),policy).unwrap();let a=h.clone();let active=thread::spawn(move||a.get(&ctx(),"r"));gate.entered();assert_eq!(h.pressure().charged_bytes,expected);let b=h.clone();let queued=thread::spawn(move||b.get(&ctx(),"r"));observe(&h,|p|p.pending_count==1);assert_eq!(h.pressure().charged_bytes,expected*2);assert_eq!(h.get(&ctx(),"r"),Err(Error::Capacity("worker pending count")));assert_eq!(h.get(&ctx(),"rr"),Err(Error::Capacity("worker request bytes")));assert_eq!(h.pressure().charged_bytes,expected*2);h.close();assert_eq!(h.get(&ctx(),"r"),Err(Error::Capacity("worker closed")));gate.release();assert_eq!(active.join().unwrap(),Err(Error::NotFound));assert_eq!(queued.join().unwrap(),Err(Error::NotFound));assert_eq!(loads.load(Ordering::SeqCst),2);drained(&h);
}

#[test]
fn expired_queued_command_has_no_store_effect(){
 let (block,gate)=gate();let store=Controlled::new();*store.load_gate.lock().unwrap()=Some(block);let canonical=store.canonical.clone();let loads=store.loads.clone();let mut policy=config();policy.queue_deadline=Duration::from_millis(100);let h=CommandWorker::new(store,grants(),Limits::default(),policy.clone()).unwrap();let a=h.clone();let active=thread::spawn(move||a.field_definitions(&ctx()));gate.entered();let b=h.clone();let queued=thread::spawn(move||b.execute(&ctx(),request("expired")));observe(&h,|p|p.pending_count==1&&p.oldest_queued_age.is_some_and(|age|age>=policy.queue_deadline));h.close();gate.release();active.join().unwrap().unwrap();assert_eq!(queued.join().unwrap(),Err(Error::Capacity("worker queue deadline before dispatch")));assert_eq!(loads.load(Ordering::SeqCst),1);assert_eq!(canonical.lock().unwrap().load("business").unwrap(),Snapshot::default());assert_eq!(h.pressure().expired,1);drained(&h);
}

#[test]
fn caller_expiry_removes_queued_command_and_keeps_dispatched_read_charged(){
 let (block,gate)=gate();let store=Controlled::new();*store.load_gate.lock().unwrap()=Some(block);let loads=store.loads.clone();let writes=store.writes.clone();let mut policy=config();policy.caller_deadline=Duration::from_millis(100);let h=CommandWorker::new(store,grants(),Limits::default(),policy).unwrap();let a=h.clone();let active=thread::spawn(move||a.field_definitions(&ctx()));gate.entered();let b=h.clone();let queued=thread::spawn(move||b.execute(&ctx(),request("expired")));assert_eq!(queued.join().unwrap(),Err(Error::Capacity("worker caller deadline before dispatch")));assert!(matches!(active.join().unwrap(),Err(Error::Storage(_))));let p=h.pressure();assert_eq!((p.pending_count,p.running_count,p.uncertain_writes),(0,1,0));assert!(p.charged_bytes>0);assert_eq!(writes.load(Ordering::SeqCst),0);h.close();gate.release();drained(&h);assert_eq!(loads.load(Ordering::SeqCst),1);
}

#[test]
fn commit_timeout_retains_capacity_and_identical_receipt_replays(){
 let (block,gate)=gate();let mut store=Controlled::new();store.committed=Some(block);let canonical=store.canonical.clone();let writes=store.writes.clone();let mut policy=config();policy.caller_deadline=Duration::from_millis(100);let h=CommandWorker::new(store,grants(),Limits::default(),policy).unwrap();let a=h.clone();let intent=request("committed");let r=intent.clone();let active=thread::spawn(move||a.execute(&ctx(),r));gate.entered();assert!(matches!(active.join().unwrap(),Err(Error::Storage(_))));let committed=canonical.lock().unwrap().load("business").unwrap();assert_eq!((committed.revision,committed.receipts.len()),(1,1));assert_eq!(committed.receipts[&intent.operation_id].request,intent);assert_eq!(h.pressure().running_count,1);assert!(h.pressure().charged_bytes>0);assert_eq!(h.pressure().uncertain_writes,1);gate.release();observe(&h,|p|p.running_count==0&&p.charged_bytes==0);assert_eq!(h.execute(&ctx(),intent.clone()),Ok(committed.receipts[&intent.operation_id].outcome.clone()));let mut changed=intent;changed.command=Command::Create(record("other"));assert_eq!(h.execute(&ctx(),changed),Err(Error::OperationReused));assert_eq!(writes.load(Ordering::SeqCst),1);assert_eq!(canonical.lock().unwrap().load("business").unwrap(),committed);assert_eq!(h.pressure().uncertain_writes,1);drained(&h);
}

#[test]
fn read_forwarding_matches_core_and_fences_pagination(){
 let store=Controlled::new();let canonical=store.canonical.clone();let h=CommandWorker::new(store,grants(),Limits::default(),config()).unwrap();for id in ["a","b"]{h.execute(&ctx(),request(id)).unwrap();}let field=FieldDefinition{key:"info".into(),label:"Info".into(),applies_to:vec![Kind::Interaction],value_type:FieldType::Text};h.execute(&ctx(),Request{operation_id:"field".into(),command:Command::DefineField(field)}).unwrap();let tier=PricingTier{id:"employee".into(),name:"Employee".into(),base:PriceBasis::B2B,discount_basis_points:100};h.execute(&ctx(),Request{operation_id:"tier".into(),command:Command::DefinePricingTier(tier)}).unwrap();let link=Relation{source:"a".into(),label:"related".into(),target:Target::Record("b".into())};h.execute(&ctx(),Request{operation_id:"link".into(),command:Command::Link(link)}).unwrap();
 let mut client=record("customer");client.kind=Kind::Customer;client.client_terms=Some(ClientTerms{classification:ClientClass::B2C,pricing:ClientPricing::Standard});let mut product=record("product");product.kind=Kind::Product;product.prices=Some(PriceBook{currency:"EUR".into(),scale:2,tax_basis:TaxBasis::Included,b2c:Some(Amount(500)),b2b:Some(Amount(400)),custom:BTreeMap::new()});for r in [client,product]{h.execute(&ctx(),Request{operation_id:format!("seed-{}",r.id),command:Command::Create(r)}).unwrap();}
 let core=Crm::new(Shared(canonical),grants(),Limits::default());assert_eq!(h.get(&ctx(),"a"),core.get(&ctx(),"a"));assert_eq!(h.field_definitions(&ctx()),core.field_definitions(&ctx()));assert_eq!(h.pricing_tiers(&ctx()),core.pricing_tiers(&ctx()));assert_eq!(h.relations(&ctx(),"a"),core.relations(&ctx(),"a"));assert_eq!(h.quote(&ctx(),"customer","product"),core.quote(&ctx(),"customer","product"));let filter=SearchFilter{tag:Some("priority".into()),kind:Some(Kind::Interaction),..Default::default()};let page=h.search_filtered(&ctx(),&filter,1,None).unwrap();assert_eq!(Ok(page.clone()),core.search_filtered(&ctx(),&filter,1,None));assert_eq!(h.search_filtered(&ctx(),&filter,1,page.next.as_ref()),core.search_filtered(&ctx(),&filter,1,page.next.as_ref()));assert_eq!(h.search(&ctx(),"a",Some("priority"),10,None),core.search(&ctx(),"a",Some("priority"),10,None));h.execute(&ctx(),request("new")).unwrap();assert_eq!(h.search_filtered(&ctx(),&filter,1,page.next.as_ref()),Err(Error::Conflict));let denied=Context{actor:"intruder".into(),..ctx()};assert_eq!(h.quote(&denied,"customer","product"),Err(Error::Denied));drained(&h);
}

#[test]
fn panic_closes_worker_and_never_dispatches_queued_command(){
 let (block,gate)=gate();let mut store=Controlled::new();store.panic_load=true;*store.load_gate.lock().unwrap()=Some(block);let loads=store.loads.clone();let writes=store.writes.clone();let h=CommandWorker::new(store,grants(),Limits::default(),config()).unwrap();let a=h.clone();let active=thread::spawn(move||a.execute(&ctx(),request("panic")));gate.entered();let b=h.clone();let queued=thread::spawn(move||b.execute(&ctx(),request("queued")));observe(&h,|p|p.pending_count==1);gate.release();assert!(matches!(active.join().unwrap(),Err(Error::Storage(_))));assert_eq!(queued.join().unwrap(),Err(Error::Capacity("worker closed before dispatch")));assert_eq!(loads.load(Ordering::SeqCst),1);assert_eq!(writes.load(Ordering::SeqCst),0);assert_eq!(h.pressure().uncertain_writes,1);assert_eq!(h.execute(&ctx(),request("closed")),Err(Error::Capacity("worker closed")));drained(&h);
}
