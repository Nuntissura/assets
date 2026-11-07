//! Lane SMB-OBSERVE-DIAG-SINK-1: `Observe::emit*` reaches the thread's `RingProducerPort`. A binding is per thread
//! (the port is one SPSC producer handle), so every test runs on its own thread by construction of the test runner.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::ring::{
    RING_DELIVERY_ERRORS_ALL, RING_OUTCOMES_ALL, RingBindRefusal, RingCode, RingIdentity, RingPhase, RingProducerPort, RingRecord,
    RingRefusal, RingTiming, RingWireState, ModuleId, ProcessRef, SessionRef, ThreadRef, bind_thread_ring, ring_of_delivery,
    ring_of_outcome,
};
use hsk_studio_observe::{
    Budget, DESCRIPTOR, DeliveryClass, DeliveryError, DiagnosticDetail, Disposition, Error, GeometryBudget, GeometryCounts,
    GeometryDetail, GeometryDisposition, GeometryRead, Observation, Observe, Outcome, Progress, Receipt, ResourceCounts,
    ResourceDetail, ResourceDisposition, ShapingBudget, ShapingCounts, ShapingDetail, ShapingDisposition, ShapingRead, SinkPort,
    TextDisposition,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

struct Counting;
thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}
fn note_allocation() {
    if ARMED.try_with(Cell::get).unwrap_or(false) {
        let _ = COUNT.try_with(|c| c.set(c.get().saturating_add(1)));
    }
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_allocation();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

const DECOY_PRIVATE: &str = "PRIVATE-PROJECT-DECOY-zqxj-WOLF";
const RESOURCE: &str = "SDOC-019abcde-0000-7000-8000-00000000beef";
const ACTOR: [&str; 6] = ["decoy-account-qwk", "decoy-principal-vbn", "decoy-owner-hjy", "decoy-owner-principal-mpz", "decoy-space-rtx", "decoy-session-lfc"];
const CORRELATION: u64 = 42;
const REVISION: u64 = 7;

fn observe(progress_events: u32) -> Observe {
    let [a, b, c, d, e, f] = ACTOR;
    Observe::new(
        CORRELATION,
        REVISION,
        DomainId::parse(RESOURCE).unwrap(),
        ActorContext::new(a, b, c, d, e, f).unwrap(),
        Budget::new(progress_events, 2048 * progress_events as usize).unwrap(),
    )
}
fn observation(outcome: Outcome) -> Observation<'static> {
    Observation {
        correlation_id: CORRELATION,
        revision: REVISION,
        outcome,
        progress: (outcome == Outcome::Progress).then(|| Progress::new(1, 2).unwrap()),
        private_project_text: Some(DECOY_PRIVATE),
    }
}
fn identity() -> RingIdentity {
    RingIdentity {
        module: ModuleId::new(7),
        thread: ThreadRef::new(3),
        session: SessionRef::new([0x11, 0x12]),
        process: ProcessRef::new([0x21, 0x22]),
    }
}
const TIMING: RingTiming = RingTiming { metric_micros: 5, timestamp_nanos: 9 };
fn clock() -> RingTiming {
    TIMING
}

struct Accept;
impl SinkPort for Accept {
    fn try_send(&mut self, _class: DeliveryClass, _bytes: &[u8]) -> Result<(), DeliveryError> {
        Ok(())
    }
}
struct Refuse(DeliveryError);
impl SinkPort for Refuse {
    fn try_send(&mut self, _class: DeliveryClass, _bytes: &[u8]) -> Result<(), DeliveryError> {
        Err(self.0)
    }
}

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<RingRecord>>>);
impl Captured {
    fn take(&self) -> Vec<RingRecord> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}
impl RingProducerPort for Captured {
    fn try_record(&mut self, record: &RingRecord) -> Result<(), RingRefusal> {
        self.0.lock().unwrap().push(*record);
        Ok(())
    }
}

/// Every emit* of one fresh emitter must record what its receipt says, with the caller's identity and clock.
#[test]
fn every_accepted_outcome_of_emit_reaches_the_bound_port_per_the_table() {
    let captured = Captured::default();
    let binding = bind_thread_ring(Box::new(captured.clone()), identity(), clock).unwrap();
    let cancel = CancellationToken::default();
    for outcome in RING_OUTCOMES_ALL {
        let receipt = observe(1).emit(observation(outcome), &cancel, &mut Accept).unwrap();
        let records = captured.take();
        assert_eq!(records.len(), 1, "{outcome:?}");
        let record = records[0];
        assert_eq!((record.code(), record.phase(), record.severity()), ring_of_outcome(outcome));
        assert_eq!(record.sequence(), receipt.sequence);
        assert_eq!(record.counters(), [u64::try_from(receipt.delivered_bytes).unwrap(), REVISION]);
        assert_eq!(record.correlation().get(), CORRELATION);
        assert_eq!(record.timing(), TIMING);
        assert_eq!((record.module(), record.thread(), record.session(), record.process()), (identity().module, identity().thread, identity().session, identity().process));
        // Identical to the phase 0 constructor over the same receipt: there is one mapping.
        assert_eq!(record, RingRecord::terminal(&receipt, identity(), TIMING));
    }
    assert_eq!(binding.state(), RingWireState { recorded: 7, saturated: 0, unavailable: 0 });
}

#[test]
fn every_emit_entry_point_records() {
    let captured = Captured::default();
    let _binding = bind_thread_ring(Box::new(captured.clone()), identity(), clock).unwrap();
    let cancel = CancellationToken::default();
    let success = observation(Outcome::Success);

    let detail = DiagnosticDetail::new(Disposition::Changed, None, None, [1, 1, 1, 1, 1]).unwrap();
    observe(0).emit_detail(success, &detail, &cancel, &mut Accept).unwrap();

    let counts = ResourceCounts::new([1, 1, 1], 1, [0, 0, 0], 0).unwrap();
    let resource = ResourceDetail::new(ResourceDisposition::Changed, None, None, 1, counts).unwrap();
    observe(0).emit_resource(success, &resource, &cancel, &mut Accept).unwrap();

    let budget = GeometryBudget { count_limit: 1, work_limit: 1, byte_limit: 1, delivery_limit: 1 };
    let counts = GeometryCounts::new([0; 8], [0; 3], 0, budget).unwrap();
    let read = GeometryRead { source_path_id: None, read_revision: None, target_revision: None, cancel_epoch: None };
    let geometry = GeometryDetail::new(GeometryDisposition::Accepted, None, None, read, counts).unwrap();
    observe(0).emit_geometry(success, &geometry, &cancel, &mut Accept).unwrap();

    let budget = ShapingBudget { counts: [1; 8], byte_limit: 1, delivery_limit: 1 };
    let counts = ShapingCounts::new([0; 8], [0; 3], 0, budget).unwrap();
    let read = ShapingRead { source_revision: None, resolver_revision: None, cancel_epoch: None, target_revision: None };
    let shaping = ShapingDetail::new(ShapingDisposition::Accepted, None, Some(TextDisposition::Shaped), None, read, counts).unwrap();
    observe(0).emit_shaping(success, &shaping, &cancel, &mut Accept).unwrap();

    let records = captured.take();
    assert_eq!(records.len(), 4);
    assert!(records.iter().all(|r| r.code() == RingCode::Success && r.phase() == RingPhase::End));
}

#[test]
fn a_canceled_token_records_the_outcome_the_receipt_reports() {
    let captured = Captured::default();
    let _binding = bind_thread_ring(Box::new(captured.clone()), identity(), clock).unwrap();
    let cancel = CancellationToken::default();
    cancel.cancel();
    let receipt = observe(0).emit(observation(Outcome::Success), &cancel, &mut Accept).unwrap();
    assert_eq!(receipt.outcome, Outcome::Canceled);
    assert_eq!(captured.take()[0].code(), RingCode::Canceled);
}

#[test]
fn a_refused_delivery_records_its_code_and_the_emit_error_is_unchanged() {
    let captured = Captured::default();
    let _binding = bind_thread_ring(Box::new(captured.clone()), identity(), clock).unwrap();
    let cancel = CancellationToken::default();
    for error in RING_DELIVERY_ERRORS_ALL {
        let mut emitter = observe(0);
        assert_eq!(emitter.emit(observation(Outcome::Success), &cancel, &mut Refuse(error)), Err(Error::Delivery(error)));
        let records = captured.take();
        assert_eq!(records.len(), 1, "{error:?}");
        assert_eq!((records[0].code(), records[0].phase(), records[0].severity()), ring_of_delivery(error));
        assert_eq!((records[0].sequence(), records[0].counters(), records[0].correlation().get()), (1, [0, 0], CORRELATION));
    }
}

#[test]
fn progress_over_the_admission_budget_records_saturation() {
    let captured = Captured::default();
    let binding = bind_thread_ring(Box::new(captured.clone()), identity(), clock).unwrap();
    let cancel = CancellationToken::default();
    let mut emitter = observe(1);
    emitter.emit(observation(Outcome::Progress), &cancel, &mut Accept).unwrap();
    assert_eq!(emitter.emit(observation(Outcome::Progress), &cancel, &mut Accept), Err(Error::Saturated));
    let records = captured.take();
    assert_eq!(records.iter().map(RingRecord::code).collect::<Vec<_>>(), [RingCode::Progress, RingCode::DeliverySaturated]);
    assert_eq!(records[1].sequence(), 2);
    assert_eq!(binding.state().recorded, 2);
}

/// A call that never reached delivery (a stale revision, a closed emitter) is no outcome: nothing is recorded.
#[test]
fn a_call_refused_before_delivery_records_nothing() {
    let captured = Captured::default();
    let _binding = bind_thread_ring(Box::new(captured.clone()), identity(), clock).unwrap();
    let cancel = CancellationToken::default();
    let mut emitter = observe(0);
    let stale = Observation { revision: REVISION + 1, ..observation(Outcome::Success) };
    assert_eq!(emitter.emit(stale, &cancel, &mut Accept), Err(Error::RevisionMismatch));
    assert!(captured.take().is_empty());
    emitter.emit(observation(Outcome::Success), &cancel, &mut Accept).unwrap();
    assert_eq!(captured.take().len(), 1);
    assert_eq!(emitter.emit(observation(Outcome::Success), &cancel, &mut Accept), Err(Error::Closed));
    assert!(captured.take().is_empty());
}

struct Refusing(RingRefusal, Arc<AtomicU64>);
impl RingProducerPort for Refusing {
    fn try_record(&mut self, _record: &RingRecord) -> Result<(), RingRefusal> {
        self.1.fetch_add(1, Ordering::Relaxed);
        Err(self.0)
    }
}

#[test]
fn a_ring_refusal_never_changes_what_emit_returns_and_is_counted() {
    let cancel = CancellationToken::default();
    let unbound: Receipt = observe(0).emit(observation(Outcome::Success), &cancel, &mut Accept).unwrap();
    for (refusal, expected) in [
        (RingRefusal::Saturated, RingWireState { recorded: 0, saturated: 2, unavailable: 0 }),
        (RingRefusal::Unavailable, RingWireState { recorded: 0, saturated: 0, unavailable: 2 }),
    ] {
        let calls = Arc::new(AtomicU64::new(0));
        let binding = bind_thread_ring(Box::new(Refusing(refusal, calls.clone())), identity(), clock).unwrap();
        let mut emitter = observe(0);
        assert_eq!(emitter.emit(observation(Outcome::Success), &cancel, &mut Accept), Ok(unbound));
        let state = emitter.state();
        assert!(state.terminal_closed && state.dropped_attempts == 0 && !state.reconciliation_required);
        let mut emitter = observe(0);
        assert_eq!(
            emitter.emit(observation(Outcome::Success), &cancel, &mut Refuse(DeliveryError::Rejected)),
            Err(Error::Delivery(DeliveryError::Rejected))
        );
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(binding.state(), expected);
    }
}

#[test]
fn the_binding_is_per_thread_one_at_a_time_and_dropping_it_unbinds() {
    let cancel = CancellationToken::default();
    let captured = Captured::default();
    let binding = bind_thread_ring(Box::new(captured.clone()), identity(), clock).unwrap();
    assert_eq!(bind_thread_ring(Box::new(Captured::default()), identity(), clock).err(), Some(RingBindRefusal::AlreadyBound));
    // Another thread has no handle: its emissions record nothing here.
    std::thread::scope(|scope| {
        scope.spawn(|| observe(0).emit(observation(Outcome::Success), &CancellationToken::default(), &mut Accept).unwrap()).join().unwrap();
    });
    assert!(captured.take().is_empty());
    observe(0).emit(observation(Outcome::Success), &cancel, &mut Accept).unwrap();
    assert_eq!(captured.take().len(), 1);
    drop(binding);
    observe(0).emit(observation(Outcome::Success), &cancel, &mut Accept).unwrap();
    assert!(captured.take().is_empty());
    // The thread can bind again afterwards.
    let again = Captured::default();
    let _binding = bind_thread_ring(Box::new(again.clone()), identity(), clock).unwrap();
    observe(0).emit(observation(Outcome::Success), &cancel, &mut Accept).unwrap();
    assert_eq!(again.take().len(), 1);
}

/// An emitter moved to another thread uses that thread's handle, never the one of the thread that built it.
#[test]
fn an_emitter_uses_the_handle_of_the_thread_it_emits_on() {
    let here = Captured::default();
    let _binding = bind_thread_ring(Box::new(here.clone()), identity(), clock).unwrap();
    let mut emitter = observe(0);
    let there = Captured::default();
    let there_port = there.clone();
    std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let _binding = bind_thread_ring(Box::new(there_port), identity(), clock).unwrap();
                emitter.emit(observation(Outcome::Success), &CancellationToken::default(), &mut Accept).unwrap();
            })
            .join()
            .unwrap();
    });
    assert!(here.take().is_empty());
    assert_eq!(there.take().len(), 1);
}

/// A port that emits from inside `try_record` must not recurse or panic: the inner emission records nothing and
/// still returns its receipt.
struct Reentrant(Arc<AtomicU64>);
impl RingProducerPort for Reentrant {
    fn try_record(&mut self, _record: &RingRecord) -> Result<(), RingRefusal> {
        self.0.fetch_add(1, Ordering::Relaxed);
        observe(0).emit(observation(Outcome::Success), &CancellationToken::default(), &mut Accept).unwrap();
        Ok(())
    }
}

#[test]
fn a_port_that_emits_does_not_recurse() {
    let calls = Arc::new(AtomicU64::new(0));
    let binding = bind_thread_ring(Box::new(Reentrant(calls.clone())), identity(), clock).unwrap();
    observe(0).emit(observation(Outcome::Success), &CancellationToken::default(), &mut Accept).unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(binding.state().recorded, 1);
}

/// Fixed preallocated producer; its one shared counter is created before the count is armed.
struct Fixed {
    slots: [Option<RingRecord>; 8],
    len: usize,
    seen: Arc<AtomicU64>,
}
impl RingProducerPort for Fixed {
    fn try_record(&mut self, record: &RingRecord) -> Result<(), RingRefusal> {
        self.seen.fetch_add(1, Ordering::Relaxed);
        match self.slots.get_mut(self.len) {
            Some(slot) => {
                *slot = Some(*record);
                self.len += 1;
                Ok(())
            }
            None => Err(RingRefusal::Saturated),
        }
    }
}

#[test]
fn a_wired_emit_allocates_nothing() {
    let cancel = CancellationToken::default();
    let seen = Arc::new(AtomicU64::new(0));
    let binding = bind_thread_ring(Box::new(Fixed { slots: [None; 8], len: 0, seen: seen.clone() }), identity(), clock).unwrap();
    let mut emitter = observe(16);
    let progress = observation(Outcome::Progress);
    COUNT.with(|c| c.set(0));
    ARMED.with(|a| a.set(true));
    for _ in 0..16 {
        emitter.emit(progress, &cancel, &mut Accept).unwrap();
    }
    ARMED.with(|a| a.set(false));
    assert_eq!(COUNT.with(Cell::get), 0, "emit through the wired path must not allocate");
    assert_eq!(seen.load(Ordering::Relaxed), 16);
    assert_eq!(binding.state(), RingWireState { recorded: 8, saturated: 8, unavailable: 0 });
    // Counterfactual: the counting allocator is live on this thread.
    ARMED.with(|a| a.set(true));
    let held = Vec::<u8>::with_capacity(16);
    ARMED.with(|a| a.set(false));
    assert_eq!(COUNT.with(Cell::get), 1);
    drop(held);
}

#[test]
fn no_text_crosses_the_wired_path() {
    thread_local! { static SEEN: RefCell<Vec<RingRecord>> = const { RefCell::new(Vec::new()) }; }
    struct Local;
    impl RingProducerPort for Local {
        fn try_record(&mut self, record: &RingRecord) -> Result<(), RingRefusal> {
            SEEN.with(|s| s.borrow_mut().push(*record));
            Ok(())
        }
    }
    let _binding = bind_thread_ring(Box::new(Local), identity(), clock).unwrap();
    let cancel = CancellationToken::default();
    for outcome in RING_OUTCOMES_ALL {
        observe(1).emit(observation(outcome), &cancel, &mut Accept).unwrap();
    }
    let texts: Vec<&str> = [DECOY_PRIVATE, RESOURCE].into_iter().chain(ACTOR).collect();
    let records = SEEN.with(|s| s.borrow().clone());
    assert_eq!(records.len(), 7);
    for record in records {
        let bytes = record.to_le_bytes();
        for text in &texts {
            for run in text.as_bytes().windows(4) {
                assert!(!bytes.windows(4).any(|w| w == run), "{text} in {record:?}");
            }
        }
    }
}

#[test]
fn the_descriptor_declares_the_wiring() {
    let ring = &DESCRIPTOR[DESCRIPTOR.find("\"ring_port\":{").unwrap()..];
    assert!(!ring.contains("not wired"), "the descriptor still says the port is not wired");
    for needle in ["bind_thread_ring", "RingClock", "tests/ring_wired.rs", "a_ring_refusal_never_changes_what_emit_returns_and_is_counted", "a_wired_emit_allocates_nothing"] {
        assert!(ring.contains(needle), "ring_port lacks {needle}");
    }
}
