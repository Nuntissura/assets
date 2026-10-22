use buurtman_balie::{
    memory::MemoryStore,
    worker::{WorkerConfig, WorkerPressure, WorkerStore},
    *,
};
use std::{
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

// Component-test capacities/deadlines only; these are not host production budgets.
fn config() -> WorkerConfig {
    WorkerConfig {
        pending_count: 1,
        max_request_bytes: 16_384,
        outstanding_bytes: 32_768,
        queue_deadline: Duration::from_secs(3),
        caller_deadline: Duration::from_secs(3),
    }
}

struct Block {
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

struct Gate {
    entered: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
}

fn gate() -> (Block, Gate) {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    (
        Block {
            entered: entered_tx,
            release: release_rx,
        },
        Gate {
            entered: entered_rx,
            release: release_tx,
        },
    )
}

impl Gate {
    fn entered(&self) {
        self.entered.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    fn release(self) {
        self.release.send(()).unwrap();
    }
}

impl Block {
    fn wait(self) {
        self.entered.send(()).unwrap();
        self.release.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}

struct ControlledStore {
    canonical: Arc<Mutex<MemoryStore>>,
    load_gate: Mutex<Option<Block>>,
    committed_gate: Option<Block>,
}

impl ControlledStore {
    fn new(load_gate: Option<Block>, committed_gate: Option<Block>) -> Self {
        Self {
            canonical: Arc::new(Mutex::new(MemoryStore::new(4))),
            load_gate: Mutex::new(load_gate),
            committed_gate,
        }
    }
}

impl Store for ControlledStore {
    fn load(&self, workspace: &str) -> Result<Snapshot, Error> {
        if let Some(block) = self.load_gate.lock().unwrap().take() {
            block.wait();
        }
        self.canonical.lock().unwrap().load(workspace)
    }

    fn compare_exchange(
        &mut self,
        workspace: &str,
        expected: u64,
        next: Snapshot,
    ) -> Result<(), Error> {
        self.canonical
            .lock()
            .unwrap()
            .compare_exchange(workspace, expected, next)?;
        if let Some(block) = self.committed_gate.take() {
            block.wait();
        }
        Ok(())
    }
}

fn observe(handle: &WorkerStore, predicate: impl Fn(&WorkerPressure) -> bool) -> WorkerPressure {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let pressure = handle.pressure();
        if predicate(&pressure) {
            return pressure;
        }
        assert!(
            Instant::now() < deadline,
            "pressure condition: {pressure:?}"
        );
        thread::yield_now();
    }
}

fn drained(handle: &WorkerStore) {
    handle.close();
    let pressure = observe(handle, |p| !p.worker_alive);
    assert_eq!(pressure.pending_count, 0);
    assert_eq!(pressure.running_count, 0);
    assert_eq!(pressure.ready_count, 0);
    assert_eq!(pressure.charged_bytes, 0);
}

fn next() -> Snapshot {
    Snapshot {
        revision: 1,
        ..Snapshot::default()
    }
}

#[test]
fn explicit_config_rejects_invalid_policy_before_start() {
    let mut policies = Vec::new();
    let mut policy = config();
    policy.pending_count = 0;
    policies.push(policy);
    let mut policy = config();
    policy.pending_count = usize::MAX;
    policies.push(policy);
    let mut policy = config();
    policy.max_request_bytes = 0;
    policies.push(policy);
    let mut policy = config();
    policy.outstanding_bytes = policy.max_request_bytes - 1;
    policies.push(policy);
    let mut policy = config();
    policy.queue_deadline = Duration::ZERO;
    policies.push(policy);
    let mut policy = config();
    policy.caller_deadline = Duration::ZERO;
    policies.push(policy);
    for policy in policies {
        assert!(matches!(
            WorkerStore::new(MemoryStore::new(1), policy),
            Err(Error::Invalid("worker config"))
        ));
    }
}

#[test]
fn cloned_handles_share_count_budget_and_close_drains_unexpired_work() {
    let (block, gate) = gate();
    let handle = WorkerStore::new(ControlledStore::new(Some(block), None), config()).unwrap();
    let first = handle.clone();
    let active = thread::spawn(move || first.load("active"));
    gate.entered();
    let second = handle.clone();
    let queued = thread::spawn(move || second.load("queued"));
    let before = observe(&handle, |p| p.pending_count == 1);
    assert_eq!(before.running_count, 1);
    assert_eq!(before.accepted, 2);
    assert!(before.charged_bytes > 0);
    let mut rejected = handle.clone();
    assert_eq!(
        rejected.compare_exchange("rejected", 0, next()),
        Err(Error::Capacity("worker pending count"))
    );
    assert_eq!(handle.pressure().charged_bytes, before.charged_bytes);
    handle.close();
    assert_eq!(handle.load("closed"), Err(Error::Capacity("worker closed")));
    gate.release();
    assert_eq!(active.join().unwrap().unwrap(), Snapshot::default());
    assert_eq!(queued.join().unwrap().unwrap(), Snapshot::default());
    let pressure = handle.pressure();
    assert_eq!(pressure.rejected, 2);
    assert_eq!(pressure.completed, 2);
    drained(&handle);
}

#[test]
fn byte_reservations_include_running_work_and_rejections_do_not_leak() {
    let (block, gate) = gate();
    let mut policy = config();
    let cas_bytes = serde_json::to_vec(&("target", 0_u64, next()))
        .unwrap()
        .len();
    policy.max_request_bytes = cas_bytes;
    policy.outstanding_bytes = cas_bytes;
    let store = ControlledStore::new(Some(block), None);
    let canonical = store.canonical.clone();
    let handle = WorkerStore::new(store, policy).unwrap();
    let first = handle.clone();
    let active = thread::spawn(move || first.load("active"));
    gate.entered();
    let charged = handle.pressure().charged_bytes;
    let mut other = handle.clone();
    assert_eq!(
        other.compare_exchange("target", 0, next()),
        Err(Error::Capacity("worker outstanding bytes"))
    );
    assert_eq!(
        handle.load(&"x".repeat(cas_bytes + 1)),
        Err(Error::Capacity("worker request bytes"))
    );
    let mut oversized = next();
    oversized.fields.insert(
        "large".into(),
        FieldDefinition {
            key: "large".into(),
            label: "x".repeat(cas_bytes),
            applies_to: vec![Kind::Product],
            value_type: FieldType::Text,
        },
    );
    assert_eq!(
        other.compare_exchange("target", 0, oversized),
        Err(Error::Capacity("worker request bytes"))
    );
    assert_eq!(handle.pressure().charged_bytes, charged);
    assert_eq!(handle.pressure().pending_count, 0);
    assert_eq!(
        canonical.lock().unwrap().load("target").unwrap().revision,
        0
    );
    gate.release();
    active.join().unwrap().unwrap();
    other.compare_exchange("target", 0, next()).unwrap();
    assert_eq!(
        canonical.lock().unwrap().load("target").unwrap().revision,
        1
    );
    drained(&handle);
}

#[test]
fn expired_queued_write_is_skipped_even_during_close_drain() {
    let (block, gate) = gate();
    let mut policy = config();
    policy.queue_deadline = Duration::from_millis(250);
    let store = ControlledStore::new(Some(block), None);
    let canonical = store.canonical.clone();
    let handle = WorkerStore::new(store, policy.clone()).unwrap();
    let first = handle.clone();
    let active = thread::spawn(move || first.load("active"));
    gate.entered();
    let mut second = handle.clone();
    let queued = thread::spawn(move || second.compare_exchange("expired", 0, next()));
    observe(&handle, |p| {
        p.pending_count == 1
            && p.oldest_queued_age
                .is_some_and(|age| age >= policy.queue_deadline)
    });
    handle.close();
    gate.release();
    active.join().unwrap().unwrap();
    assert_eq!(
        queued.join().unwrap(),
        Err(Error::Capacity("worker queue deadline before dispatch"))
    );
    assert_eq!(
        canonical.lock().unwrap().load("expired").unwrap().revision,
        0
    );
    assert_eq!(handle.pressure().expired, 1);
    drained(&handle);
}

#[test]
fn caller_expiry_removes_queued_write_but_keeps_running_capacity() {
    let (block, gate) = gate();
    let mut policy = config();
    policy.caller_deadline = Duration::from_millis(500);
    let store = ControlledStore::new(Some(block), None);
    let canonical = store.canonical.clone();
    let handle = WorkerStore::new(store, policy).unwrap();
    let first = handle.clone();
    let active = thread::spawn(move || first.load("active"));
    gate.entered();
    let mut second = handle.clone();
    let queued = thread::spawn(move || second.compare_exchange("expired", 0, next()));
    assert_eq!(
        queued.join().unwrap(),
        Err(Error::Capacity("worker caller deadline before dispatch"))
    );
    assert_eq!(
        active.join().unwrap(),
        Err(Error::Storage("worker read deadline after dispatch".into()))
    );
    let pressure = handle.pressure();
    assert_eq!(pressure.pending_count, 0);
    assert_eq!(pressure.running_count, 1);
    assert!(pressure.charged_bytes > 0);
    assert_eq!(pressure.timed_out, 2);
    assert_eq!(pressure.expired, 1);
    assert_eq!(pressure.uncertain_writes, 0);
    assert_eq!(
        canonical.lock().unwrap().load("expired").unwrap().revision,
        0
    );
    handle.close();
    assert!(handle.pressure().worker_alive);
    gate.release();
    drained(&handle);
}

struct Grants;
impl Authorization for Grants {
    fn allows(&self, context: &Context, _access: Access) -> bool {
        context.workspace == "business" && context.actor == "operator"
    }
}

#[test]
fn crm_reconciles_commit_after_caller_timeout_without_second_mutation() {
    let (block, gate) = gate();
    let mut policy = config();
    policy.caller_deadline = Duration::from_millis(500);
    let store = ControlledStore::new(None, Some(block));
    let canonical = store.canonical.clone();
    let handle = WorkerStore::new(store, policy).unwrap();
    let context = Context {
        workspace: "business".into(),
        actor: "operator".into(),
    };
    let request = Request {
        operation_id: "define".into(),
        command: Command::DefineField(FieldDefinition {
            key: "info".into(),
            label: "Info".into(),
            applies_to: vec![Kind::Product],
            value_type: FieldType::Text,
        }),
    };
    let first = handle.clone();
    let ctx = context.clone();
    let intent = request.clone();
    let active =
        thread::spawn(move || Crm::new(first, Grants, Limits::default()).execute(&ctx, intent));
    gate.entered();
    assert_eq!(
        active.join().unwrap(),
        Err(Error::Storage(
            "worker write outcome uncertain: caller deadline after dispatch".into()
        ))
    );
    let committed = canonical.lock().unwrap().load("business").unwrap();
    let expected = Outcome {
        workspace_revision: 1,
        record_revision: None,
    };
    assert_eq!(committed.revision, 1);
    assert_eq!(committed.receipts.len(), 1);
    assert_eq!(committed.receipts["define"].actor, context.actor);
    assert_eq!(committed.receipts["define"].request, request);
    assert_eq!(committed.receipts["define"].outcome, expected);
    assert_eq!(handle.pressure().running_count, 1);
    assert!(handle.pressure().charged_bytes > 0);
    assert_eq!(handle.pressure().uncertain_writes, 1);
    gate.release();
    observe(&handle, |p| p.running_count == 0 && p.charged_bytes == 0);
    assert_eq!(handle.pressure().uncertain_writes, 1);
    let mut crm = Crm::new(handle.clone(), Grants, Limits::default());
    assert_eq!(crm.execute(&context, request.clone()), Ok(expected));
    let changed = Request {
        operation_id: request.operation_id,
        command: Command::DefineField(FieldDefinition {
            key: "different".into(),
            label: "Different".into(),
            applies_to: vec![Kind::Product],
            value_type: FieldType::Text,
        }),
    };
    assert_eq!(crm.execute(&context, changed), Err(Error::OperationReused));
    assert_eq!(
        canonical.lock().unwrap().load("business").unwrap(),
        committed
    );
    drained(&handle);
}

struct FailingStore {
    cas_gate: Option<Block>,
}
impl Store for FailingStore {
    fn load(&self, _workspace: &str) -> Result<Snapshot, Error> {
        Err(Error::Storage("host unavailable".into()))
    }

    fn compare_exchange(
        &mut self,
        workspace: &str,
        _expected: u64,
        _next: Snapshot,
    ) -> Result<(), Error> {
        if let Some(block) = self.cas_gate.take() {
            block.wait();
        }
        if workspace == "uncertain" {
            Err(Error::Storage("host write outcome unknown".into()))
        } else {
            Err(Error::Conflict)
        }
    }
}

#[test]
fn store_errors_propagate_without_retries_or_reservation_leaks() {
    let mut handle = WorkerStore::new(FailingStore { cas_gate: None }, config()).unwrap();
    assert_eq!(
        handle.load("business"),
        Err(Error::Storage("host unavailable".into()))
    );
    assert_eq!(
        handle.compare_exchange("business", 0, next()),
        Err(Error::Conflict)
    );
    assert_eq!(handle.pressure().uncertain_writes, 0);
    assert_eq!(
        handle.compare_exchange("uncertain", 0, next()),
        Err(Error::Storage("host write outcome unknown".into()))
    );
    assert_eq!(handle.pressure().uncertain_writes, 1);
    assert_eq!(handle.pressure().accepted, 3);
    assert_eq!(handle.pressure().completed, 3);
    assert_eq!(handle.pressure().charged_bytes, 0);
    drained(&handle);
}

struct PanickingStore {
    gate: Mutex<Option<Block>>,
}
impl Store for PanickingStore {
    fn load(&self, _workspace: &str) -> Result<Snapshot, Error> {
        if let Some(block) = self.gate.lock().unwrap().take() {
            block.wait();
        }
        panic!("host Store panic")
    }

    fn compare_exchange(
        &mut self,
        _workspace: &str,
        _expected: u64,
        _next: Snapshot,
    ) -> Result<(), Error> {
        if let Some(block) = self.gate.lock().unwrap().take() {
            block.wait();
        }
        panic!("host CAS panic")
    }
}

#[test]
fn panicking_store_closes_admission_and_releases_current_reservation() {
    let (block, gate) = gate();
    let handle = WorkerStore::new(
        PanickingStore {
            gate: Mutex::new(Some(block)),
        },
        config(),
    )
    .unwrap();
    let first = handle.clone();
    let active = thread::spawn(move || first.load("business"));
    gate.entered();
    let mut second = handle.clone();
    let queued = thread::spawn(move || second.compare_exchange("never-started", 0, next()));
    observe(&handle, |p| p.pending_count == 1);
    gate.release();
    assert_eq!(
        active.join().unwrap(),
        Err(Error::Storage(
            "worker Store panicked; outcome uncertain".into()
        ))
    );
    assert_eq!(
        queued.join().unwrap(),
        Err(Error::Capacity("worker closed before dispatch"))
    );
    assert_eq!(handle.pressure().completed, 1);
    assert_eq!(handle.pressure().uncertain_writes, 0);
    assert_eq!(handle.load("closed"), Err(Error::Capacity("worker closed")));
    drained(&handle);
}

#[test]
fn cas_panic_counts_uncertain_write_after_return() {
    let mut handle = WorkerStore::new(
        PanickingStore {
            gate: Mutex::new(None),
        },
        config(),
    )
    .unwrap();
    assert_eq!(
        handle.compare_exchange("uncertain", 0, next()),
        Err(Error::Storage(
            "worker Store panicked; outcome uncertain".into()
        ))
    );
    assert_eq!(handle.pressure().uncertain_writes, 1);
    assert_eq!(handle.pressure().charged_bytes, 0);
    drained(&handle);
}

#[test]
fn storage_error_after_dispatched_timeout_counts_same_cas_once() {
    let (block, gate) = gate();
    let mut policy = config();
    policy.caller_deadline = Duration::from_millis(500);
    let handle = WorkerStore::new(
        FailingStore {
            cas_gate: Some(block),
        },
        policy,
    )
    .unwrap();
    let mut first = handle.clone();
    let active = thread::spawn(move || first.compare_exchange("uncertain", 0, next()));
    gate.entered();
    assert_eq!(
        active.join().unwrap(),
        Err(Error::Storage(
            "worker write outcome uncertain: caller deadline after dispatch".into()
        ))
    );
    assert_eq!(handle.pressure().uncertain_writes, 1);
    assert_eq!(handle.pressure().running_count, 1);
    gate.release();
    observe(&handle, |p| p.completed == 1 && p.charged_bytes == 0);
    assert_eq!(handle.pressure().uncertain_writes, 1);
    drained(&handle);
}
