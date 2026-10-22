//! Independent hostile boundaries: dispatch grants, external CAS and lost replies.
use buurtman_balie::{
    memory::MemoryStore,
    worker::{CommandWorker, WorkerConfig, WorkerPressure},
    *,
};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

struct Hold {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}
struct Gate {
    entered: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
}
fn gate() -> (Hold, Gate) {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    (
        Hold {
            entered: entered_tx,
            release: release_rx,
        },
        Gate {
            entered: entered_rx,
            release: release_tx,
        },
    )
}
impl Hold {
    fn wait(self) {
        self.entered.send(()).unwrap();
        self.release.recv_timeout(Duration::from_secs(8)).unwrap();
    }
}
impl Gate {
    fn entered(&self) {
        self.entered.recv_timeout(Duration::from_secs(8)).unwrap();
    }
    fn release(self) {
        self.release.send(()).unwrap();
    }
}

#[derive(Clone)]
struct Canonical(Arc<Mutex<MemoryStore>>);
impl Canonical {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(MemoryStore::new(2))))
    }
    fn snapshot(&self) -> Snapshot {
        self.0.lock().unwrap().load("business").unwrap()
    }
}
impl Store for Canonical {
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

#[derive(Default)]
struct Observations {
    loads: AtomicUsize,
    cas: AtomicUsize,
    before_cas: Mutex<Option<Hold>>,
    after_commit: Mutex<Option<Hold>>,
}
struct ObservedStore {
    canonical: Canonical,
    observed: Arc<Observations>,
}
impl Store for ObservedStore {
    fn load(&self, workspace: &str) -> Result<Snapshot, Error> {
        self.observed.loads.fetch_add(1, Ordering::SeqCst);
        self.canonical.load(workspace)
    }
    fn compare_exchange(
        &mut self,
        workspace: &str,
        expected: u64,
        next: Snapshot,
    ) -> Result<(), Error> {
        self.observed.cas.fetch_add(1, Ordering::SeqCst);
        let before = self.observed.before_cas.lock().unwrap().take();
        if let Some(hold) = before {
            hold.wait();
        }
        self.canonical.compare_exchange(workspace, expected, next)?;
        let after = self.observed.after_commit.lock().unwrap().take();
        if let Some(hold) = after {
            hold.wait();
        }
        Ok(())
    }
}
struct Permit;
impl Authorization for Permit {
    fn allows(&self, context: &Context, _: Access) -> bool {
        context.workspace == "business"
    }
}
type AuthObservation = (String, Access, bool);
struct CurrentGrants {
    enabled: Arc<AtomicBool>,
    calls: Arc<Mutex<Vec<AuthObservation>>>,
}
impl Authorization for CurrentGrants {
    fn allows(&self, context: &Context, access: Access) -> bool {
        let allowed = context.workspace == "business"
            && (context.actor != "revokable" || self.enabled.load(Ordering::SeqCst));
        self.calls
            .lock()
            .unwrap()
            .push((context.actor.clone(), access, allowed));
        allowed
    }
}
fn context(actor: &str) -> Context {
    Context {
        workspace: "business".into(),
        actor: actor.into(),
    }
}
fn request(id: &str, command: Command) -> Request {
    Request {
        operation_id: id.into(),
        command,
    }
}
fn field(id: &str) -> Command {
    Command::DefineField(FieldDefinition {
        key: id.into(),
        label: id.into(),
        value_type: FieldType::Text,
        applies_to: vec![Kind::Product],
    })
}
fn product() -> Record {
    Record {
        id: "camera".into(),
        kind: Kind::Product,
        name: "Camera".into(),
        description: String::new(),
        tags: vec![],
        fields: BTreeMap::new(),
        case_status: None,
        client_terms: None,
        prices: None,
        revision: 0,
    }
}
fn config() -> WorkerConfig {
    WorkerConfig {
        pending_count: 4,
        max_request_bytes: 16_384,
        outstanding_bytes: 65_536,
        queue_deadline: Duration::from_secs(5),
        caller_deadline: Duration::from_secs(5),
    }
}
fn observe(worker: &CommandWorker, condition: impl Fn(&WorkerPressure) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let pressure = worker.pressure();
        if condition(&pressure) {
            return;
        }
        assert!(Instant::now() < deadline, "pressure boundary: {pressure:?}");
        thread::yield_now();
    }
}
fn drained(worker: &CommandWorker) {
    worker.close();
    observe(worker, |p| !p.worker_alive);
    let pressure = worker.pressure();
    assert_eq!(
        (
            pressure.pending_count,
            pressure.running_count,
            pressure.ready_count,
            pressure.charged_bytes
        ),
        (0, 0, 0, 0)
    );
}

#[test]
fn queued_revocation_denies_new_write_read_and_persisted_receipt_before_storage() {
    let canonical = Canonical::new();
    let revokable = context("revokable");
    let prior = request("prior", Command::Create(product()));
    let expected_prior = Crm::new(canonical.clone(), Permit, Limits::default())
        .execute(&revokable, prior.clone())
        .unwrap();
    let initial = canonical.snapshot();
    let observed = Arc::new(Observations::default());
    let (hold, blocker) = gate();
    *observed.before_cas.lock().unwrap() = Some(hold);
    let enabled = Arc::new(AtomicBool::new(true));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let worker = CommandWorker::new(
        ObservedStore {
            canonical: canonical.clone(),
            observed: observed.clone(),
        },
        CurrentGrants {
            enabled: enabled.clone(),
            calls: calls.clone(),
        },
        Limits::default(),
        config(),
    )
    .unwrap();
    let active = worker.clone();
    let first = thread::spawn(move || {
        active.execute(&context("operator"), request("blocker", field("blocker")))
    });
    blocker.entered();
    let write_worker = worker.clone();
    let write = thread::spawn(move || {
        write_worker.execute(
            &context("revokable"),
            request("denied-new", field("denied-new")),
        )
    });
    let read_worker = worker.clone();
    let read = thread::spawn(move || read_worker.get(&context("revokable"), "camera"));
    let replay_worker = worker.clone();
    let replay_intent = prior.clone();
    let replay = thread::spawn(move || replay_worker.execute(&context("revokable"), replay_intent));
    observe(&worker, |p| p.pending_count == 3 && p.running_count == 1);
    assert_eq!(
        *calls.lock().unwrap(),
        vec![("operator".into(), Access::Write, true)]
    );
    enabled.store(false, Ordering::SeqCst);
    blocker.release();
    assert!(first.join().unwrap().is_ok());
    assert_eq!(write.join().unwrap(), Err(Error::Denied));
    assert_eq!(read.join().unwrap(), Err(Error::Denied));
    assert_eq!(replay.join().unwrap(), Err(Error::Denied));
    let auth = calls.lock().unwrap().clone();
    assert_eq!(auth.len(), 4);
    assert_eq!(
        auth.iter()
            .filter(|(actor, _, allowed)| actor == "revokable" && !allowed)
            .count(),
        3
    );
    assert_eq!(observed.loads.load(Ordering::SeqCst), 1);
    assert_eq!(observed.cas.load(Ordering::SeqCst), 1);
    let after = canonical.snapshot();
    assert_eq!(after.revision, initial.revision + 1);
    assert_eq!(after.records, initial.records);
    assert_eq!(after.receipts["prior"], initial.receipts["prior"]);
    assert_eq!(after.receipts.len(), initial.receipts.len() + 1);
    assert!(!after.fields.contains_key("denied-new"));
    assert!(!after.receipts.contains_key("denied-new"));
    enabled.store(true, Ordering::SeqCst);
    assert_eq!(worker.execute(&revokable, prior), Ok(expected_prior));
    assert_eq!(canonical.snapshot(), after);
    assert_eq!(observed.cas.load(Ordering::SeqCst), 1);
    drained(&worker);
}

#[test]
fn external_writer_conflict_and_committed_lost_reply_preserve_canonical_intent() {
    let canonical = Canonical::new();
    let ctx = context("operator");
    let mut external = Crm::new(canonical.clone(), Permit, Limits::default());
    external
        .execute(&ctx, request("seed", Command::Create(product())))
        .unwrap();
    let old = external.get(&ctx, "camera").unwrap();
    let observed = Arc::new(Observations::default());
    let (hold, blocker) = gate();
    *observed.before_cas.lock().unwrap() = Some(hold);
    let mut policy = config();
    policy.caller_deadline = Duration::from_millis(500);
    let worker = CommandWorker::new(
        ObservedStore {
            canonical: canonical.clone(),
            observed: observed.clone(),
        },
        Permit,
        Limits::default(),
        policy,
    )
    .unwrap();
    let mut stale = old.clone();
    stale.name = "Queued stale edit".into();
    let active = worker.clone();
    let stale_intent = request("raced", Command::Update(stale.clone()));
    let concurrent = thread::spawn(move || active.execute(&context("operator"), stale_intent));
    blocker.entered();
    let mut edited = old;
    edited.name = "External canonical edit".into();
    external
        .execute(&ctx, request("external", Command::Update(edited)))
        .unwrap();
    let external_state = canonical.snapshot();
    blocker.release();
    assert_eq!(concurrent.join().unwrap(), Err(Error::Conflict));
    assert_eq!(canonical.snapshot(), external_state);
    assert_eq!(observed.loads.load(Ordering::SeqCst), 1);
    assert_eq!(observed.cas.load(Ordering::SeqCst), 1);
    assert_eq!(
        worker.execute(&ctx, request("stale-record", Command::Update(stale))),
        Err(Error::Conflict)
    );
    assert_eq!(observed.cas.load(Ordering::SeqCst), 1);
    assert_eq!(canonical.snapshot(), external_state);

    let (hold, committed) = gate();
    *observed.after_commit.lock().unwrap() = Some(hold);
    let mut fresh = external_state.records["camera"].clone();
    fresh.name = "Committed before response loss".into();
    let intent = request("lost-response", Command::Update(fresh));
    let active = worker.clone();
    let immutable = intent.clone();
    let lost = thread::spawn(move || active.execute(&context("operator"), immutable));
    committed.entered();
    let outcome = Outcome {
        workspace_revision: external_state.revision + 1,
        record_revision: Some(external_state.records["camera"].revision + 1),
    };
    let committed_state = canonical.snapshot();
    assert_eq!(committed_state.revision, outcome.workspace_revision);
    assert_eq!(
        committed_state.records["camera"].revision,
        outcome.record_revision.unwrap()
    );
    assert_eq!(committed_state.receipts["lost-response"].actor, ctx.actor);
    assert_eq!(committed_state.receipts["lost-response"].request, intent);
    assert_eq!(committed_state.receipts["lost-response"].outcome, outcome);
    assert!(
        matches!(lost.join().unwrap(), Err(Error::Storage(message)) if message.contains("outcome uncertain"))
    );
    let pressure = worker.pressure();
    assert_eq!(pressure.running_count, 1);
    assert!(pressure.charged_bytes > 0);
    assert_eq!(pressure.uncertain_writes, 1);
    let replay_worker = worker.clone();
    let immutable = intent.clone();
    let replay = thread::spawn(move || replay_worker.execute(&context("operator"), immutable));
    observe(&worker, |p| p.pending_count == 1 && p.running_count == 1);
    committed.release();
    assert_eq!(replay.join().unwrap(), Ok(outcome));
    assert_eq!(observed.cas.load(Ordering::SeqCst), 2);
    assert_eq!(canonical.snapshot(), committed_state);
    assert_eq!(
        worker.execute(&context("other"), intent.clone()),
        Err(Error::OperationReused)
    );
    assert_eq!(
        worker.execute(&ctx, request(&intent.operation_id, field("changed-intent"))),
        Err(Error::OperationReused)
    );
    assert_eq!(observed.cas.load(Ordering::SeqCst), 2);
    assert_eq!(canonical.snapshot(), committed_state);
    drained(&worker);
}
