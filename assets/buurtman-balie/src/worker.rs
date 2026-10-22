//! One host-supplied Store on one persistent thread, shared by synchronous handles.
//! Admission is volatile. Hosts retain write intent, transport deadlines, response
//! byte limits, authorization and aggregate cross-process admission ownership.
use crate::{
    Authorization, Context, Crm, Cursor, Error, FieldDefinition, Limits, Outcome, Page,
    PricingTier, Quote, Record, Relation, Request, SearchFilter, Snapshot, Store,
};
use std::{
    collections::VecDeque,
    io::{self, Write},
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant},
};

/// Required host policy; no production budgets or latency SLA are inferred.
#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub pending_count: usize,
    pub max_request_bytes: usize,
    /// Submitted bytes held by queued, running and not-yet-consumed replies.
    /// This is not a bound on snapshots returned by load or total process memory.
    pub outstanding_bytes: usize,
    pub queue_deadline: Duration,
    pub caller_deadline: Duration,
}

#[derive(Clone, Debug)]
pub struct WorkerPressure {
    pub config: WorkerConfig,
    pub admission_open: bool,
    pub worker_alive: bool,
    pub pending_count: usize,
    pub running_count: usize,
    pub ready_count: usize,
    pub charged_bytes: usize,
    pub oldest_queued_age: Option<Duration>,
    pub accepted: u64,
    pub rejected: u64,
    pub expired: u64,
    pub completed: u64,
    pub timed_out: u64,
    pub uncertain_writes: u64,
}

/// Cloneable handles share one queue and one in-flight call, not independent stores.
/// Methods block their calling thread; async hosts must use a bounded blocking bridge.
pub struct WorkerStore {
    shared: Arc<Shared>,
}

struct Shared {
    config: WorkerConfig,
    state: Mutex<State>,
    available: Condvar,
}

struct State {
    queue: VecDeque<Job>,
    open: bool,
    alive: bool,
    handles: usize,
    running: usize,
    ready: usize,
    charged: usize,
    accepted: u64,
    rejected: u64,
    expired: u64,
    completed: u64,
    timed_out: u64,
    uncertain_writes: u64,
}

struct Job {
    operation: Operation,
    call: Arc<Call>,
    enqueued: Instant,
    dispatch_deadline: Instant,
}

enum Operation {
    Load(String),
    Cas(String, u64, Box<Snapshot>),
    Execute(Context, Box<Request>),
    Get(Context, String),
    Search(Context, SearchFilter, usize, Option<Cursor>),
    Quote(Context, String, String),
    Fields(Context),
    Tiers(Context),
    Relations(Context, String),
}

enum Reply {
    Loaded(Box<Snapshot>),
    Written,
    Executed(Outcome),
    Record(Box<Record>),
    Page(Box<Page>),
    Quote(Quote),
    Fields(Vec<FieldDefinition>),
    Tiers(Vec<PricingTier>),
    Relations(Vec<Relation>),
}

struct Call {
    phase: Mutex<Phase>,
    changed: Condvar,
    bytes: usize,
    write: bool,
}

enum Phase {
    Queued,
    Running,
    Abandoned,
    Ready(Option<Result<Reply, Error>>),
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

impl WorkerStore {
    pub fn new<S: Store + Send + 'static>(store: S, config: WorkerConfig) -> Result<Self, Error> {
        let mut store = store;
        Self::new_handler(config, move |operation| match operation {
            Operation::Load(workspace) => store
                .load(&workspace)
                .map(|value| Reply::Loaded(Box::new(value))),
            Operation::Cas(workspace, expected, next) => {
                store.compare_exchange(&workspace, expected, *next)?;
                Ok(Reply::Written)
            }
            _ => unreachable!("Store worker operation"),
        })
    }

    fn new_handler<F>(config: WorkerConfig, handler: F) -> Result<Self, Error>
    where
        F: FnMut(Operation) -> Result<Reply, Error> + Send + 'static,
    {
        let now = Instant::now();
        if config.pending_count == 0
            || config.pending_count.checked_add(1).is_none()
            || config.max_request_bytes == 0
            || config.outstanding_bytes < config.max_request_bytes
            || config.queue_deadline.is_zero()
            || config.caller_deadline.is_zero()
            || now.checked_add(config.queue_deadline).is_none()
            || now.checked_add(config.caller_deadline).is_none()
        {
            return Err(Error::Invalid("worker config"));
        }
        let shared = Arc::new(Shared {
            config,
            state: Mutex::new(State {
                queue: VecDeque::new(),
                open: true,
                alive: true,
                handles: 1,
                running: 0,
                ready: 0,
                charged: 0,
                accepted: 0,
                rejected: 0,
                expired: 0,
                completed: 0,
                timed_out: 0,
                uncertain_writes: 0,
            }),
            available: Condvar::new(),
        });
        let worker = shared.clone();
        // No join on Drop: an owned synchronous Store may remain blocked until its
        // host transport deadline. The worker is observable and exits after drain.
        thread::Builder::new()
            .name("buurtman-store-worker".into())
            .spawn(move || run(handler, worker))
            .map_err(|_| Error::Storage("worker thread startup failed".into()))?;
        Ok(Self { shared })
    }

    /// Stops new admission; the worker drains accepted work only before its queue
    /// deadline, then exits. Does not join, force-cancel or undo an in-flight commit.
    /// Dropping the last handle also closes admission. Observe worker_alive for exit.
    pub fn close(&self) {
        lock(&self.shared.state).open = false;
        self.shared.available.notify_all();
    }

    pub fn pressure(&self) -> WorkerPressure {
        let state = lock(&self.shared.state);
        WorkerPressure {
            config: self.shared.config.clone(),
            admission_open: state.open,
            worker_alive: state.alive,
            pending_count: state.queue.len(),
            running_count: state.running,
            ready_count: state.ready,
            charged_bytes: state.charged,
            oldest_queued_age: state.queue.front().map(|job| job.enqueued.elapsed()),
            accepted: state.accepted,
            rejected: state.rejected,
            expired: state.expired,
            completed: state.completed,
            timed_out: state.timed_out,
            uncertain_writes: state.uncertain_writes,
        }
    }

    fn reject(&self, reason: &'static str) -> Error {
        let mut state = lock(&self.shared.state);
        state.rejected = state.rejected.saturating_add(1);
        Error::Capacity(reason)
    }

    fn submit(&self, operation: Operation) -> Result<Reply, Error> {
        let started = Instant::now();
        let config = &self.shared.config;
        // Count the exact JSON tuple without allocating a serialized payload:
        // load [workspace]; CAS [workspace, expected, next_snapshot]. Not RPC bytes.
        let mut counter = Counter {
            bytes: 0,
            limit: config.max_request_bytes,
        };
        let encoded = match &operation {
            Operation::Load(workspace) => serde_json::to_writer(&mut counter, &(workspace,)),
            Operation::Cas(workspace, expected, next) => {
                serde_json::to_writer(&mut counter, &(workspace, expected, next))
            }
            _ => unreachable!("command operations use counted admission"),
        };
        if encoded.is_err() {
            return Err(self.reject("worker request bytes"));
        }
        self.submit_counted(operation, counter.bytes, started)
    }

    fn submit_counted(
        &self,
        operation: Operation,
        bytes: usize,
        started: Instant,
    ) -> Result<Reply, Error> {
        let config = &self.shared.config;
        let caller_end = started
            .checked_add(config.caller_deadline)
            .ok_or(Error::Invalid("worker deadline"))?;
        let queue_end = started
            .checked_add(config.queue_deadline)
            .ok_or(Error::Invalid("worker deadline"))?;
        let call = Arc::new(Call {
            phase: Mutex::new(Phase::Queued),
            changed: Condvar::new(),
            bytes,
            write: matches!(operation, Operation::Cas(..) | Operation::Execute(..)),
        });
        {
            let mut state = lock(&self.shared.state);
            let reason = if !state.open {
                Some("worker closed")
            } else if state.queue.len() >= config.pending_count
                || state.queue.len() + state.running + state.ready > config.pending_count
            {
                Some("worker pending count")
            } else if state
                .charged
                .checked_add(call.bytes)
                .is_none_or(|bytes| bytes > config.outstanding_bytes)
            {
                Some("worker outstanding bytes")
            } else if Instant::now() >= queue_end.min(caller_end) {
                Some("worker deadline before admission")
            } else {
                None
            };
            if let Some(reason) = reason {
                state.rejected = state.rejected.saturating_add(1);
                return Err(Error::Capacity(reason));
            }
            state.charged += call.bytes;
            state.accepted = state.accepted.saturating_add(1);
            state.queue.push_back(Job {
                operation,
                call: call.clone(),
                enqueued: started,
                dispatch_deadline: queue_end.min(caller_end),
            });
        }
        self.shared.available.notify_one();
        loop {
            let mut phase = lock(&call.phase);
            if !matches!(*phase, Phase::Ready(_)) && Instant::now() < caller_end {
                let remaining = caller_end.saturating_duration_since(Instant::now());
                phase = call
                    .changed
                    .wait_timeout(phase, remaining)
                    .unwrap_or_else(|error| error.into_inner())
                    .0;
            }
            // All transitions use shared -> call lock order, including expiry.
            drop(phase);
            let mut state = lock(&self.shared.state);
            let mut phase = lock(&call.phase);
            if let Phase::Ready(reply) = &mut *phase {
                state.ready -= 1;
                state.charged -= call.bytes;
                return reply.take().expect("one caller per worker submission");
            }
            if Instant::now() < caller_end {
                continue;
            }
            state.timed_out = state.timed_out.saturating_add(1);
            if matches!(*phase, Phase::Queued) {
                state.queue.retain(|job| !Arc::ptr_eq(&job.call, &call));
                state.charged -= call.bytes;
                state.expired = state.expired.saturating_add(1);
                *phase = Phase::Abandoned;
                return Err(Error::Capacity("worker caller deadline before dispatch"));
            }
            *phase = Phase::Abandoned;
            return if call.write {
                state.uncertain_writes = state.uncertain_writes.saturating_add(1);
                Err(Error::Storage(
                    "worker write outcome uncertain: caller deadline after dispatch".into(),
                ))
            } else {
                Err(Error::Storage("worker read deadline after dispatch".into()))
            };
        }
    }
}

impl Clone for WorkerStore {
    fn clone(&self) -> Self {
        lock(&self.shared.state).handles += 1;
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl Drop for WorkerStore {
    fn drop(&mut self) {
        let mut state = lock(&self.shared.state);
        state.handles -= 1;
        if state.handles == 0 {
            state.open = false;
            self.shared.available.notify_all();
        }
    }
}

impl Store for WorkerStore {
    fn load(&self, workspace: &str) -> Result<Snapshot, Error> {
        if workspace.len() > self.shared.config.max_request_bytes {
            return Err(self.reject("worker request bytes"));
        }
        match self.submit(Operation::Load(workspace.into()))? {
            Reply::Loaded(snapshot) => Ok(*snapshot),
            _ => unreachable!("load reply"),
        }
    }

    fn compare_exchange(
        &mut self,
        workspace: &str,
        expected: u64,
        next: Snapshot,
    ) -> Result<(), Error> {
        if workspace.len() > self.shared.config.max_request_bytes {
            return Err(self.reject("worker request bytes"));
        }
        match self.submit(Operation::Cas(workspace.into(), expected, Box::new(next)))? {
            Reply::Written => Ok(()),
            _ => unreachable!("CAS reply"),
        }
    }
}

/// Whole-command admission. One worker owns one CRM over the supplied raw Store.
/// Clones share admission; external writers still require CAS. No operation is retried.
/// Admission is volatile and caller timeout cannot cancel dispatched effects.
#[derive(Clone)]
pub struct CommandWorker {
    engine: WorkerStore,
}

impl CommandWorker {
    pub fn new<S, A>(
        store: S,
        authorization: A,
        limits: Limits,
        config: WorkerConfig,
    ) -> Result<Self, Error>
    where
        S: Store + Send + 'static,
        A: Authorization + Send + 'static,
    {
        let mut crm = Crm::new(store, authorization, limits);
        let engine = WorkerStore::new_handler(config, move |operation| match operation {
            Operation::Execute(ctx, request) => crm.execute(&ctx, *request).map(Reply::Executed),
            Operation::Get(ctx, id) => crm
                .get(&ctx, &id)
                .map(|record| Reply::Record(Box::new(record))),
            Operation::Search(ctx, filter, limit, cursor) => crm
                .search_filtered(&ctx, &filter, limit, cursor.as_ref())
                .map(|page| Reply::Page(Box::new(page))),
            Operation::Quote(ctx, client, product) => {
                crm.quote(&ctx, &client, &product).map(Reply::Quote)
            }
            Operation::Fields(ctx) => crm.field_definitions(&ctx).map(Reply::Fields),
            Operation::Tiers(ctx) => crm.pricing_tiers(&ctx).map(Reply::Tiers),
            Operation::Relations(ctx, id) => crm.relations(&ctx, &id).map(Reply::Relations),
            _ => unreachable!("CRM worker operation"),
        })?;
        Ok(Self { engine })
    }

    pub fn close(&self) {
        self.engine.close();
    }
    pub fn pressure(&self) -> WorkerPressure {
        self.engine.pressure()
    }

    fn submit<T, F>(&self, args: &T, operation: F) -> Result<Reply, Error>
    where
        T: serde::Serialize,
        F: FnOnce() -> Operation,
    {
        let started = Instant::now();
        let mut counter = Counter {
            bytes: 0,
            limit: self.engine.shared.config.max_request_bytes,
        };
        serde_json::to_writer(&mut counter, args)
            .map_err(|_| self.engine.reject("worker request bytes"))?;
        // Clone owned arguments only after the bounded serializer accepts them.
        self.engine
            .submit_counted(operation(), counter.bytes, started)
    }

    /// Charges JSON tuple `(Context, Request)`; authorization runs at dispatch.
    pub fn execute(&self, ctx: &Context, request: Request) -> Result<Outcome, Error> {
        match self.submit(&(ctx, &request), || {
            Operation::Execute(ctx.clone(), Box::new(request.clone()))
        })? {
            Reply::Executed(outcome) => Ok(outcome),
            _ => unreachable!("execute reply"),
        }
    }

    pub fn get(&self, ctx: &Context, id: &str) -> Result<Record, Error> {
        match self.submit(&(ctx, id), || Operation::Get(ctx.clone(), id.into()))? {
            Reply::Record(record) => Ok(*record),
            _ => unreachable!("get reply"),
        }
    }

    pub fn search_filtered(
        &self,
        ctx: &Context,
        filter: &SearchFilter,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<Page, Error> {
        match self.submit(&(ctx, filter, limit, cursor), || {
            Operation::Search(ctx.clone(), filter.clone(), limit, cursor.cloned())
        })? {
            Reply::Page(page) => Ok(*page),
            _ => unreachable!("search reply"),
        }
    }

    pub fn search(
        &self,
        ctx: &Context,
        query: &str,
        tag: Option<&str>,
        limit: usize,
        cursor: Option<&Cursor>,
    ) -> Result<Page, Error> {
        match self.submit(&(ctx, query, tag, limit, cursor), || {
            Operation::Search(
                ctx.clone(),
                SearchFilter {
                    query: query.into(),
                    tag: tag.map(str::to_owned),
                    ..Default::default()
                },
                limit,
                cursor.cloned(),
            )
        })? {
            Reply::Page(page) => Ok(*page),
            _ => unreachable!("search reply"),
        }
    }

    pub fn quote(&self, ctx: &Context, client_id: &str, product_id: &str) -> Result<Quote, Error> {
        match self.submit(&(ctx, client_id, product_id), || {
            Operation::Quote(ctx.clone(), client_id.into(), product_id.into())
        })? {
            Reply::Quote(quote) => Ok(quote),
            _ => unreachable!("quote reply"),
        }
    }

    pub fn field_definitions(&self, ctx: &Context) -> Result<Vec<FieldDefinition>, Error> {
        match self.submit(&(ctx,), || Operation::Fields(ctx.clone()))? {
            Reply::Fields(fields) => Ok(fields),
            _ => unreachable!("fields reply"),
        }
    }

    pub fn pricing_tiers(&self, ctx: &Context) -> Result<Vec<PricingTier>, Error> {
        match self.submit(&(ctx,), || Operation::Tiers(ctx.clone()))? {
            Reply::Tiers(tiers) => Ok(tiers),
            _ => unreachable!("tiers reply"),
        }
    }

    pub fn relations(&self, ctx: &Context, id: &str) -> Result<Vec<Relation>, Error> {
        match self.submit(&(ctx, id), || Operation::Relations(ctx.clone(), id.into()))? {
            Reply::Relations(relations) => Ok(relations),
            _ => unreachable!("relations reply"),
        }
    }
}

fn ready(state: &mut State, call: &Call, result: Result<Reply, Error>) {
    let mut phase = lock(&call.phase);
    if matches!(*phase, Phase::Abandoned) {
        state.charged -= call.bytes;
    } else {
        // Dispatched timeout already counted this CAS while marking Abandoned.
        // Storage includes a caught Store panic and can follow an actual commit.
        if call.write && matches!(result, Err(Error::Storage(_))) {
            state.uncertain_writes = state.uncertain_writes.saturating_add(1);
        }
        state.ready += 1;
        *phase = Phase::Ready(Some(result));
        call.changed.notify_one();
    }
}

fn run<F: FnMut(Operation) -> Result<Reply, Error>>(mut handler: F, shared: Arc<Shared>) {
    loop {
        let job = {
            let mut state = lock(&shared.state);
            loop {
                if let Some(job) = state.queue.pop_front() {
                    if Instant::now() >= job.dispatch_deadline {
                        state.expired = state.expired.saturating_add(1);
                        ready(
                            &mut state,
                            &job.call,
                            Err(Error::Capacity("worker queue deadline before dispatch")),
                        );
                        continue;
                    }
                    *lock(&job.call.phase) = Phase::Running;
                    state.running = 1;
                    break job;
                }
                if !state.open {
                    state.alive = false;
                    return;
                }
                state = shared
                    .available
                    .wait(state)
                    .unwrap_or_else(|error| error.into_inner());
            }
        };
        let result = catch_unwind(AssertUnwindSafe(|| handler(job.operation)));
        let mut state = lock(&shared.state);
        state.running = 0;
        state.completed = state.completed.saturating_add(1);
        match result {
            Ok(result) => ready(&mut state, &job.call, result),
            Err(_) => {
                ready(
                    &mut state,
                    &job.call,
                    Err(Error::Storage(
                        "worker Store panicked; outcome uncertain".into(),
                    )),
                );
                state.open = false;
                state.alive = false;
                while let Some(queued) = state.queue.pop_front() {
                    ready(
                        &mut state,
                        &queued.call,
                        Err(Error::Capacity("worker closed before dispatch")),
                    );
                }
                return;
            }
        }
    }
}

struct Counter {
    bytes: usize,
    limit: usize,
}

impl Write for Counter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self
            .bytes
            .checked_add(bytes.len())
            .filter(|next| *next <= self.limit)
            .ok_or_else(|| io::Error::other("worker request bytes"))?;
        self.bytes = next;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
