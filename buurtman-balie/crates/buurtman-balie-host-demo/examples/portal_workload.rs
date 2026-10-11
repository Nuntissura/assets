//! P1 temporary Portal diagnostic workload; no production SLA or durability claim.
use buurtman_balie::{
    memory::MemoryStore,
    worker::{CommandWorker, WorkerConfig, WorkerPressure, WorkerStore},
    *,
};
use serde_json::{json, Value as Json};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Barrier, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

const PROFILE: &str = "BB-PORTAL-DIAGNOSTIC-P1";
const COMMAND_PROFILE: &str = "BB-PORTAL-DIAGNOSTIC-P2";
const WORKSPACE: &str = "portal-diagnostic";
struct Auth;
impl Authorization for Auth {
    fn allows(&self, ctx: &Context, _: Access) -> bool {
        ctx.workspace == WORKSPACE && ctx.actor == "diagnostic"
    }
}
fn context() -> Context {
    Context {
        workspace: WORKSPACE.into(),
        actor: "diagnostic".into(),
    }
}
fn config(queue: u64, caller: u64) -> WorkerConfig {
    WorkerConfig {
        pending_count: 16,
        max_request_bytes: 1_048_576,
        outstanding_bytes: 8_388_608,
        queue_deadline: Duration::from_millis(queue),
        caller_deadline: Duration::from_millis(caller),
    }
}
fn record(id: String, kind: Kind) -> Record {
    Record {
        name: format!("Portal {id}"),
        id,
        client_terms: (kind == Kind::Customer).then_some(ClientTerms {
            classification: ClientClass::B2C,
            pricing: ClientPricing::Standard,
        }),
        prices: None,
        case_status: (kind == Kind::SupportCase).then_some(CaseStatus::New),
        kind,
        description: "d".repeat(256),
        tags: vec!["portal".into(), "diagnostic".into()],
        fields: BTreeMap::new(),
        revision: 0,
    }
}
fn request(id: String, command: Command) -> Request {
    Request {
        operation_id: id,
        command,
    }
}
fn outcome<T>(result: &Result<T, Error>) -> &'static str {
    match result {
        Ok(_) => "success",
        Err(Error::Conflict) => "conflict",
        Err(Error::Capacity(_)) => "capacity",
        Err(Error::Storage(_)) => "storage",
        Err(_) => "other",
    }
}
fn pressure(worker: &WorkerStore) -> Json {
    pressure_value(worker.pressure())
}
fn pressure_value(p: WorkerPressure) -> Json {
    json!({"pending":p.pending_count,"running":p.running_count,"ready":p.ready_count,"charged_bytes":p.charged_bytes,
        "accepted":p.accepted,"rejected":p.rejected,"expired":p.expired,"completed":p.completed,
        "timed_out":p.timed_out,"uncertain_writes":p.uncertain_writes,"worker_alive":p.worker_alive})
}
#[derive(Clone)]
struct SharedMemory(Arc<Mutex<MemoryStore>>);
impl Store for SharedMemory {
    fn load(&self, workspace: &str) -> Result<Snapshot, Error> {
        self.0.lock().unwrap().load(workspace)
    }
    fn compare_exchange(
        &mut self,
        workspace: &str,
        expected: u64,
        next: Snapshot,
    ) -> Result<(), Error> {
        self.0
            .lock()
            .unwrap()
            .compare_exchange(workspace, expected, next)
    }
}
#[derive(Clone)]
enum Workload {
    Store(WorkerStore),
    Command(CommandWorker, SharedMemory),
}
impl Workload {
    fn pressure(&self) -> WorkerPressure {
        match self {
            Self::Store(w) => w.pressure(),
            Self::Command(w, _) => w.pressure(),
        }
    }
    fn load(&self, workspace: &str) -> Result<Snapshot, Error> {
        match self {
            Self::Store(w) => w.load(workspace),
            Self::Command(_, memory) => memory.load(workspace),
        }
    }
    fn close(&self) {
        match self {
            Self::Store(w) => w.close(),
            Self::Command(w, _) => w.close(),
        }
    }
    fn client(&self, bytes: Arc<Mutex<Bytes>>) -> Client {
        match self {
            Self::Store(w) => Client::Store(Box::new(Crm::new(
                Metered {
                    worker: w.clone(),
                    bytes,
                },
                Auth,
                Limits::default(),
            ))),
            Self::Command(w, _) => Client::Command(w.clone(), bytes),
        }
    }
}
enum Client {
    Store(Box<Crm<Metered, Auth>>),
    Command(CommandWorker, Arc<Mutex<Bytes>>),
}
fn count_command(bytes: &Arc<Mutex<Bytes>>, size: usize) {
    let mut m = bytes.lock().unwrap();
    m.submitted += size;
    m.maximum = m.maximum.max(size);
    m.calls += 1;
}
impl Client {
    fn execute(&mut self, ctx: &Context, request: Request) -> Result<Outcome, Error> {
        match self {
            Self::Store(crm) => crm.execute(ctx, request),
            Self::Command(w, bytes) => {
                count_command(bytes, serde_json::to_vec(&(ctx, &request)).unwrap().len());
                w.execute(ctx, request)
            }
        }
    }
    fn get(&self, ctx: &Context, id: &str) -> Result<Record, Error> {
        match self {
            Self::Store(crm) => crm.get(ctx, id),
            Self::Command(w, bytes) => {
                count_command(bytes, serde_json::to_vec(&(ctx, id)).unwrap().len());
                w.get(ctx, id)
            }
        }
    }
    fn search(
        &self,
        ctx: &Context,
        query: &str,
        tag: Option<&str>,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<Page, Error> {
        match self {
            Self::Store(crm) => crm.search(ctx, query, tag, limit, cursor),
            Self::Command(w, bytes) => {
                let filter = SearchFilter {
                    query: query.into(),
                    tag: tag.map(str::to_owned),
                    ..Default::default()
                };
                count_command(
                    bytes,
                    serde_json::to_vec(&(ctx, &filter, limit, cursor))
                        .unwrap()
                        .len(),
                );
                w.search_filtered(ctx, &filter, limit, cursor)
            }
        }
    }
}
fn summary(snapshot: &Snapshot) -> Json {
    let mut kinds = BTreeMap::<String, usize>::new();
    for item in snapshot.records.values() {
        *kinds.entry(format!("{:?}", item.kind)).or_default() += 1;
    }
    json!({"records":snapshot.records.len(),"relations":snapshot.relations.len(),"receipts":snapshot.receipts.len(),"revision":snapshot.revision,"kinds":kinds})
}
#[derive(Default)]
struct Bytes {
    submitted: usize,
    maximum: usize,
    calls: usize,
}
#[derive(Clone)]
struct Metered {
    worker: WorkerStore,
    bytes: Arc<Mutex<Bytes>>,
}
impl Metered {
    fn count(&self, bytes: usize) {
        let mut m = self.bytes.lock().unwrap();
        m.submitted += bytes;
        m.maximum = m.maximum.max(bytes);
        m.calls += 1;
    }
}
impl Store for Metered {
    fn load(&self, workspace: &str) -> Result<Snapshot, Error> {
        self.count(serde_json::to_vec(&(workspace,)).unwrap().len());
        self.worker.load(workspace)
    }
    fn compare_exchange(
        &mut self,
        workspace: &str,
        expected: u64,
        next: Snapshot,
    ) -> Result<(), Error> {
        self.count(
            serde_json::to_vec(&(workspace, expected, &next))
                .unwrap()
                .len(),
        );
        self.worker.compare_exchange(workspace, expected, next)
    }
}
fn wait_idle(worker: &Workload) -> Result<(), String> {
    let start = Instant::now();
    while worker.pressure().running_count != 0 || worker.pressure().pending_count != 0 {
        if start.elapsed() > Duration::from_secs(5) {
            return Err("worker did not drain".into());
        }
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}
fn seed(worker: &Workload) -> Result<Snapshot, String> {
    let mut crm = worker.client(Arc::new(Mutex::new(Bytes::default())));
    for i in 0..100 {
        let kind = match i {
            0..=9 => Kind::Organization,
            10..=49 => Kind::Customer,
            50..=74 => Kind::SupportCase,
            _ => Kind::Interaction,
        };
        let mut item = record(format!("seed-{i}"), kind);
        if (10..50).contains(&i) && i % 2 == 1 {
            item.client_terms.as_mut().unwrap().classification = ClientClass::B2B;
        }
        crm.execute(
            &context(),
            request(format!("seed-create-{i}"), Command::Create(item)),
        )
        .map_err(|e| format!("seed: {e:?}"))?;
    }
    for i in 0..50 {
        crm.execute(
            &context(),
            request(
                format!("seed-link-{i}"),
                Command::Link(Relation {
                    source: format!("seed-{i}"),
                    label: "related".into(),
                    target: Target::Record(format!("seed-{}", i + 50)),
                }),
            ),
        )
        .map_err(|e| format!("link: {e:?}"))?;
    }
    for i in 0..10 {
        crm.execute(
            &context(),
            request(
                format!("seed-ref-{i}"),
                Command::Link(Relation {
                    source: format!("seed-{i}"),
                    label: "account".into(),
                    target: Target::External(ExternalRef {
                        owner: "Portal".into(),
                        resource_type: "account".into(),
                        id: format!("opaque-{i}"),
                    }),
                }),
            ),
        )
        .map_err(|e| format!("ref: {e:?}"))?;
    }
    worker
        .load(WORKSPACE)
        .map_err(|e| format!("seed reread: {e:?}"))
}
fn phase(worker: &Workload, concurrency: usize, phase: usize) -> Result<Json, String> {
    let before = worker
        .load(WORKSPACE)
        .map_err(|e| format!("before: {e:?}"))?;
    let before_pressure = pressure_value(worker.pressure());
    let bytes = Arc::new(Mutex::new(Bytes::default()));
    let barrier = Arc::new(Barrier::new(concurrency + 1));
    let stop = Arc::new(AtomicBool::new(false));
    let sample_worker = worker.clone();
    let sample_stop = stop.clone();
    let sampler = thread::spawn(move || {
        let mut maxima = [0usize; 4];
        while !sample_stop.load(Ordering::Acquire) {
            let p = sample_worker.pressure();
            for (slot, value) in maxima.iter_mut().zip([
                p.pending_count,
                p.running_count,
                p.ready_count,
                p.charged_bytes,
            ]) {
                *slot = (*slot).max(value);
            }
            thread::sleep(Duration::from_millis(1));
        }
        maxima
    });
    let mut jobs = Vec::new();
    for caller in 0..concurrency {
        let mut crm = worker.client(bytes.clone());
        let start = barrier.clone();
        jobs.push(thread::spawn(move || {
            let mut operations = Vec::new(); start.wait();
            for index in 0..100 {
                let now = Instant::now();
                let (kind,result) = if index % 10 < 6 {
                    ("search",crm.search(&context(), "portl", Some("portal"), 25, None).map(|page| { assert!(page.hits.len() <= 25); }))
                } else if index % 10 < 9 {
                    ("read",crm.get(&context(), &format!("seed-{}",index%100)).map(|_| ()))
                } else {
                    let id = format!("phase-{phase}-caller-{caller}-interaction-{index}");
                    ("create",crm.execute(&context(),request(format!("create-{id}"),Command::Create(record(id,Kind::Interaction)))).map(|_| ()))
                };
                operations.push(json!({"kind":kind,"outcome":outcome(&result),"latency_us":now.elapsed().as_micros(),"operation_id":(kind=="create").then(||format!("create-phase-{phase}-caller-{caller}-interaction-{index}")),"error":result.err().map(|e|format!("{e:?}"))}));
            }
            operations
        }));
    }
    let now = Instant::now();
    barrier.wait();
    let mut operations = Vec::new();
    for job in jobs {
        operations.extend(job.join().map_err(|_| "caller panic")?);
    }
    let wall = now.elapsed().as_micros();
    stop.store(true, Ordering::Release);
    let maxima = sampler.join().map_err(|_| "sampler panic")?;
    wait_idle(worker)?;
    let after = worker
        .load(WORKSPACE)
        .map_err(|e| format!("after: {e:?}"))?;
    let mut counts = BTreeMap::<String, BTreeMap<String, usize>>::new();
    for op in &operations {
        *counts
            .entry(op["kind"].as_str().unwrap().into())
            .or_default()
            .entry(op["outcome"].as_str().unwrap().into())
            .or_default() += 1;
    }
    let successes = counts
        .get("create")
        .and_then(|c| c.get("success"))
        .copied()
        .unwrap_or(0);
    let committed = after.receipts.len() - before.receipts.len();
    let uncertain = counts
        .get("create")
        .and_then(|c| c.get("storage"))
        .copied()
        .unwrap_or(0);
    if operations.len() != concurrency * 100
        || committed < successes
        || committed > successes + uncertain
        || after.records.len() != before.records.len() + committed
        || after.revision != before.revision + committed as u64
        || after.relations != before.relations
    {
        return Err("phase canonical counters differ".into());
    }
    for op in operations.iter().filter(|op| op["kind"] == "create") {
        let recorded = after
            .receipts
            .contains_key(op["operation_id"].as_str().unwrap());
        if (op["outcome"] == "success" && !recorded)
            || (recorded && op["outcome"] != "success" && op["outcome"] != "storage")
        {
            return Err("rejected creation mutated canonical state".into());
        }
    }
    if operations.iter().any(|op| op["outcome"] == "other") {
        return Err("unexpected phase error".into());
    }
    let measured = bytes.lock().unwrap();
    Ok(
        json!({"concurrency":concurrency,"wall_us":wall,"operations":operations,"counts":counts,
        "canonical":summary(&after),"submitted_bytes":measured.submitted,"max_request_bytes":measured.maximum,"submission_calls":measured.calls,
        "submission_unit":match worker { Workload::Store(_) => "Store calls", Workload::Command(..) => "CRM commands" },
        "pressure_before":before_pressure,"pressure_after":pressure_value(worker.pressure()),"sampled_pressure_max":{"pending":maxima[0],"running":maxima[1],"ready":maxima[2],"charged_bytes":maxima[3]}}),
    )
}
struct FaultStore {
    memory: Arc<Mutex<MemoryStore>>,
    slow_load: bool,
    lose_commit: Arc<AtomicBool>,
    cas: Arc<Mutex<usize>>,
}
impl Store for FaultStore {
    fn load(&self, workspace: &str) -> Result<Snapshot, Error> {
        if self.slow_load {
            thread::sleep(Duration::from_millis(250));
        }
        self.memory.lock().unwrap().load(workspace)
    }
    fn compare_exchange(
        &mut self,
        workspace: &str,
        expected: u64,
        next: Snapshot,
    ) -> Result<(), Error> {
        *self.cas.lock().unwrap() += 1;
        self.memory
            .lock()
            .unwrap()
            .compare_exchange(workspace, expected, next)?;
        if self.lose_commit.swap(false, Ordering::AcqRel) {
            thread::sleep(Duration::from_millis(250));
            return Err(Error::Storage("injected committed response loss".into()));
        }
        Ok(())
    }
}
fn faults() -> Result<Json, String> {
    let memory = Arc::new(Mutex::new(MemoryStore::new(1)));
    let cas = Arc::new(Mutex::new(0));
    let mut expiry = WorkerStore::new(
        FaultStore {
            memory: memory.clone(),
            slow_load: true,
            lose_commit: Arc::new(AtomicBool::new(false)),
            cas: cas.clone(),
        },
        config(50, 100),
    )
    .map_err(|e| format!("fault worker: {e:?}"))?;
    let first = expiry.clone();
    let job = thread::spawn(move || {
        let now = Instant::now();
        (first.load(WORKSPACE), now.elapsed().as_micros())
    });
    let start = Instant::now();
    while expiry.pressure().running_count == 0 {
        if start.elapsed() > Duration::from_secs(2) {
            return Err("delayed load did not start".into());
        }
        thread::sleep(Duration::from_millis(1));
    }
    let candidate = Snapshot {
        revision: 1,
        ..Snapshot::default()
    };
    let now = Instant::now();
    let queued = expiry.compare_exchange(WORKSPACE, 0, candidate);
    let queued_us = now.elapsed().as_micros();
    let (first_result, load_us) = job.join().map_err(|_| "fault load panic")?;
    wait_idle(&Workload::Store(expiry.clone()))?;
    let canonical = memory
        .lock()
        .unwrap()
        .load(WORKSPACE)
        .map_err(|e| format!("fault reread: {e:?}"))?;
    if !matches!(queued, Err(Error::Capacity(_)))
        || !matches!(first_result, Err(Error::Storage(_)))
        || *cas.lock().unwrap() != 0
        || canonical != Snapshot::default()
        || expiry.pressure().expired == 0
    {
        return Err("queue expiry invariant".into());
    }
    let expired = json!({"verified":true,"load_outcome":outcome(&first_result),"load_latency_us":load_us,"queued_cas_outcome":outcome(&queued),"queued_cas_latency_us":queued_us,"pressure":pressure(&expiry),"canonical":summary(&canonical),"cas_dispatches":0});
    expiry.close();
    let memory = Arc::new(Mutex::new(MemoryStore::new(1)));
    let cas = Arc::new(Mutex::new(0));
    let loss = WorkerStore::new(
        FaultStore {
            memory: memory.clone(),
            slow_load: false,
            lose_commit: Arc::new(AtomicBool::new(true)),
            cas: cas.clone(),
        },
        config(50, 100),
    )
    .map_err(|e| format!("loss worker: {e:?}"))?;
    let mut crm = Crm::new(loss.clone(), Auth, Limits::default());
    let intent = request(
        "response-loss".into(),
        Command::Create(record("fault-interaction".into(), Kind::Interaction)),
    );
    let now = Instant::now();
    let initial = crm.execute(&context(), intent.clone());
    let initial_us = now.elapsed().as_micros();
    let timeout_pressure = pressure(&loss);
    wait_idle(&Workload::Store(loss.clone()))?;
    let committed = memory
        .lock()
        .unwrap()
        .load(WORKSPACE)
        .map_err(|e| format!("commit reread: {e:?}"))?;
    let now = Instant::now();
    let replay = crm.execute(&context(), intent.clone());
    let replay_us = now.elapsed().as_micros();
    let final_snapshot = memory
        .lock()
        .unwrap()
        .load(WORKSPACE)
        .map_err(|e| format!("replay reread: {e:?}"))?;
    let mut changed = intent;
    changed.command = Command::Create(record("different".into(), Kind::Interaction));
    let changed_result = crm.execute(&context(), changed);
    if !matches!(initial, Err(Error::Storage(_)))
        || replay.as_ref().ok() != committed.receipts.get("response-loss").map(|r| &r.outcome)
        || committed != final_snapshot
        || committed.records.len() != 1
        || committed.receipts.len() != 1
        || *cas.lock().unwrap() != 1
        || !matches!(changed_result, Err(Error::OperationReused))
        || timeout_pressure["running"] != 1
        || timeout_pressure["uncertain_writes"] != 1
    {
        return Err("response loss/replay invariant".into());
    }
    let lost = json!({"verified":true,"initial_outcome":outcome(&initial),"initial_latency_us":initial_us,"replay_outcome":outcome(&replay),"replay_latency_us":replay_us,"changed_intent":"OperationReused","timeout_pressure":timeout_pressure,"pressure_after":pressure(&loss),"canonical":summary(&final_snapshot),"identical_replay_unchanged":committed==final_snapshot,"cas_dispatches":1});
    loss.close();
    Ok(
        json!({"queue_expiry":expired,"response_loss":lost,"injected_delay_ms":250,"queue_deadline_ms":50,"caller_deadline_ms":100}),
    )
}
fn faults_command() -> Result<Json, String> {
    let memory = Arc::new(Mutex::new(MemoryStore::new(1)));
    let cas = Arc::new(Mutex::new(0));
    let expiry = CommandWorker::new(
        FaultStore {
            memory: memory.clone(),
            slow_load: true,
            lose_commit: Arc::new(AtomicBool::new(false)),
            cas: cas.clone(),
        },
        Auth,
        Limits::default(),
        config(50, 100),
    )
    .map_err(|e| format!("command expiry worker: {e:?}"))?;
    let first = expiry.clone();
    let job = thread::spawn(move || {
        let now = Instant::now();
        (first.get(&context(), "missing"), now.elapsed().as_micros())
    });
    let start = Instant::now();
    while expiry.pressure().running_count == 0 {
        if start.elapsed() > Duration::from_secs(2) {
            return Err("delayed command did not start".into());
        }
        thread::sleep(Duration::from_millis(1));
    }
    let now = Instant::now();
    let queued = expiry.execute(
        &context(),
        request(
            "expired".into(),
            Command::Create(record("expired".into(), Kind::Interaction)),
        ),
    );
    let queued_us = now.elapsed().as_micros();
    let (first_result, load_us) = job.join().map_err(|_| "fault command panic")?;
    wait_idle(&Workload::Command(
        expiry.clone(),
        SharedMemory(memory.clone()),
    ))?;
    let canonical = memory
        .lock()
        .unwrap()
        .load(WORKSPACE)
        .map_err(|e| format!("expiry reread: {e:?}"))?;
    if !matches!(queued, Err(Error::Capacity(_)))
        || !matches!(first_result, Err(Error::Storage(_)))
        || *cas.lock().unwrap() != 0
        || canonical != Snapshot::default()
        || expiry.pressure().expired == 0
    {
        return Err("command queue expiry invariant".into());
    }
    let expired = json!({"verified":true,"load_outcome":outcome(&first_result),"load_latency_us":load_us,
        "queued_cas_outcome":outcome(&queued),"queued_cas_latency_us":queued_us,
        "pressure":pressure_value(expiry.pressure()),"canonical":summary(&canonical),"cas_dispatches":0});
    expiry.close();
    let memory = Arc::new(Mutex::new(MemoryStore::new(1)));
    let cas = Arc::new(Mutex::new(0));
    let loss = CommandWorker::new(
        FaultStore {
            memory: memory.clone(),
            slow_load: false,
            lose_commit: Arc::new(AtomicBool::new(true)),
            cas: cas.clone(),
        },
        Auth,
        Limits::default(),
        config(50, 100),
    )
    .map_err(|e| format!("command loss worker: {e:?}"))?;
    let intent = request(
        "response-loss".into(),
        Command::Create(record("fault-interaction".into(), Kind::Interaction)),
    );
    let now = Instant::now();
    let initial = loss.execute(&context(), intent.clone());
    let initial_us = now.elapsed().as_micros();
    let timeout_pressure = pressure_value(loss.pressure());
    wait_idle(&Workload::Command(
        loss.clone(),
        SharedMemory(memory.clone()),
    ))?;
    let committed = memory
        .lock()
        .unwrap()
        .load(WORKSPACE)
        .map_err(|e| format!("commit reread: {e:?}"))?;
    let now = Instant::now();
    let replay = loss.execute(&context(), intent.clone());
    let replay_us = now.elapsed().as_micros();
    let mut changed = intent;
    changed.command = Command::Create(record("different".into(), Kind::Interaction));
    let changed_result = loss.execute(&context(), changed);
    let final_snapshot = memory
        .lock()
        .unwrap()
        .load(WORKSPACE)
        .map_err(|e| format!("replay reread: {e:?}"))?;
    if !matches!(initial, Err(Error::Storage(_)))
        || replay.as_ref().ok() != committed.receipts.get("response-loss").map(|r| &r.outcome)
        || committed != final_snapshot
        || committed.records.len() != 1
        || committed.receipts.len() != 1
        || *cas.lock().unwrap() != 1
        || !matches!(changed_result, Err(Error::OperationReused))
        || timeout_pressure["running"] != 1
        || timeout_pressure["uncertain_writes"] != 1
    {
        return Err("command response loss/replay invariant".into());
    }
    let lost = json!({"verified":true,"initial_outcome":outcome(&initial),"initial_latency_us":initial_us,
        "replay_outcome":outcome(&replay),"replay_latency_us":replay_us,"changed_intent":"OperationReused",
        "timeout_pressure":timeout_pressure,"pressure_after":pressure_value(loss.pressure()),"canonical":summary(&final_snapshot),
        "identical_replay_unchanged":committed==final_snapshot,"cas_dispatches":1});
    loss.close();
    Ok(
        json!({"queue_expiry":expired,"response_loss":lost,"injected_delay_ms":250,"queue_deadline_ms":50,"caller_deadline_ms":100}),
    )
}
fn run(command_mode: bool) -> Result<Json, String> {
    let worker = if command_mode {
        let memory = SharedMemory(Arc::new(Mutex::new(MemoryStore::new(1))));
        let worker = CommandWorker::new(memory.clone(), Auth, Limits::default(), config(250, 1000))
            .map_err(|e| format!("command worker: {e:?}"))?;
        Workload::Command(worker, memory)
    } else {
        Workload::Store(
            WorkerStore::new(MemoryStore::new(1), config(250, 1000))
                .map_err(|e| format!("worker: {e:?}"))?,
        )
    };
    let seeded = seed(&worker)?;
    if seeded.records.len() != 100
        || seeded.relations.len() != 60
        || seeded.receipts.len() != 160
        || seeded
            .records
            .values()
            .any(|r| r.description.len() != 256 || !r.description.is_ascii() || r.tags.len() != 2)
    {
        return Err("seed invariant".into());
    }
    let mut phases = Vec::new();
    for (index, concurrency) in [1, 4, 16].into_iter().enumerate() {
        phases.push(phase(&worker, concurrency, index)?);
    }
    let final_snapshot = worker
        .load(WORKSPACE)
        .map_err(|e| format!("final reread: {e:?}"))?;
    let result = json!({"schema_version":"buurtman.portal-workload.v1","profile_id":if command_mode {COMMAND_PROFILE} else {PROFILE},"passed":true,"failures":[],"total_operations":2100,"backend":"temporary MemoryStore","canonical_read_scope":if command_mode {"direct shared raw MemoryStore after command drain"} else {"Store worker snapshot"},"submitted_bytes_scope":if command_mode {"attempted Context/command argument JSON tuples including admission rejections; not RPC or total memory"} else {"attempted Store load/CAS JSON tuples including admission rejections; not RPC or total memory"},"worker_config":{"pending":16,"max_submitted_request_bytes":1048576,"outstanding_bytes":8388608,"queued_deadline_ms":250,"caller_deadline_ms":1000},"service_limits":{"snapshot_bytes":Limits::default().snapshot_bytes,"operation_count":Limits::default().operations},"seed":summary(&seeded),"seed_description_ascii_bytes":256,"seed_record_links":50,"seed_opaque_portal_account_refs":10,"phases":phases,"final_snapshot":summary(&final_snapshot),"raw_canonical":command_mode.then_some(&final_snapshot),"faults":if command_mode {faults_command()?} else {faults()?}});
    worker.close();
    Ok(result)
}
fn main() {
    let command_mode = std::env::args().skip(1).any(|arg| arg == "--command");
    let profile = if command_mode {
        COMMAND_PROFILE
    } else {
        PROFILE
    };
    let result = run(command_mode);
    let mut failed = result.is_err();
    let output = result.unwrap_or_else(|error|json!({"schema_version":"buurtman.portal-workload.v1","profile_id":profile,"passed":false,"failures":[error]}));
    let mut encoded = output.to_string();
    if encoded.len() > 1_048_576 {
        failed = true;
        encoded=json!({"schema_version":"buurtman.portal-workload.v1","profile_id":profile,"passed":false,"failures":["diagnostic output exceeds 1 MiB"]}).to_string();
    }
    println!("{encoded}");
    io::stdout().flush().unwrap();
    // Monitor owns this process and records CPU/peak working set before releasing it.
    let mut release = String::new();
    let _ = io::stdin().read_line(&mut release);
    if failed {
        std::process::exit(1);
    }
}
