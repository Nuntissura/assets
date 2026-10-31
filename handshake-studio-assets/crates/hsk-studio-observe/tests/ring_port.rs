//! RF-04 phase 0 (SMB-OBSERVE-RING-PORT-1): ring-shaped, text-free producer port beside SinkPort.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::ring::{
    CorrelationRef, ModuleId, ProcessRef, RING_CORE_BYTES, RING_DELIVERY_ERRORS_ALL, RING_OUTCOMES_ALL,
    RING_RECORD_BYTES, RING_RECORD_OFFSETS, RingCode, RingIdentity, RingKind, RingPhase, RingProducerPort,
    RingRecord, RingRefusal, RingSeverity, RingTiming, STUDIO_CODE_BLOCK_FIRST, STUDIO_CODE_BLOCK_LAST,
    SessionRef, ThreadRef, host_mirror, ring_of_delivery, ring_of_outcome,
};
use hsk_studio_observe::{
    Budget, DESCRIPTOR, DeliveryClass, DeliveryError, Error, FailureCode, Observation, Observe, Outcome,
    Progress, Receipt, SinkPort,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

// Counting allocator: counts allocations made by the current thread while it is armed (thread-local, so other
// test threads never disturb a count). Const-initialised thread locals without drop glue do not allocate.
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
fn allocations_during<R>(f: impl FnOnce() -> R) -> (R, usize) {
    COUNT.with(|c| c.set(0));
    ARMED.with(|a| a.set(true));
    let result = f();
    ARMED.with(|a| a.set(false));
    (result, COUNT.with(Cell::get))
}

/// Handshake `src/frontend/handshake_native/diag_ring/src/schema.rs` @292d7a1c `DiagEvent` layout,
/// (offset, size): event_code, phase_marker, severity, _reserved, thread_id, sequence_id, counter_a,
/// counter_b, metric_micros, timestamp_nanos; total 56, align 8. Contract mirror, not a dependency.
const DIAG_EVENT_CORE: [(usize, usize); 10] =
    [(0, 2), (2, 1), (3, 1), (4, 4), (8, 8), (16, 8), (24, 8), (32, 8), (40, 8), (48, 8)];
/// The full RingRecord table of the module docs: the core above, then module, reserved_tail, correlation,
/// session, process.
const RING_TABLE: [(usize, usize); 15] = [
    (0, 2),
    (2, 1),
    (3, 1),
    (4, 4),
    (8, 8),
    (16, 8),
    (24, 8),
    (32, 8),
    (40, 8),
    (48, 8),
    (56, 4),
    (60, 4),
    (64, 8),
    (72, 16),
    (88, 16),
];
/// Pinned discriminants (PROPOSED Studio block; Handshake has not allocated it).
const PINNED: [(RingCode, u16); 18] = [
    (RingCode::Progress, 0x5300),
    (RingCode::Success, 0x5301),
    (RingCode::FailureValidation, 0x5302),
    (RingCode::FailureUnavailable, 0x5303),
    (RingCode::FailureLoss, 0x5304),
    (RingCode::FailureUnsupported, 0x5305),
    (RingCode::Canceled, 0x5306),
    (RingCode::DeliverySaturated, 0x5310),
    (RingCode::DeliveryRejected, 0x5311),
    (RingCode::DeliveryUnavailable, 0x5312),
    (RingCode::DeliveryIndeterminate, 0x5313),
    (RingCode::GpuUnavailable, 0x5320),
    (RingCode::GpuDeviceLost, 0x5321),
    (RingCode::GpuAdmissionDenied, 0x5322),
    (RingCode::GpuBudgetExceeded, 0x5323),
    (RingCode::GpuDoubleRelease, 0x5324),
    (RingCode::GpuLateRelease, 0x5325),
    (RingCode::GpuRebound, 0x5326),
];

// Test-local exhaustive readings (no wildcard): a new Outcome / FailureCode / DeliveryError variant stops this
// file compiling, and the ALL arrays are checked against these cases.
fn outcome_case(outcome: Outcome) -> (usize, RingKind) {
    use RingPhase::{End, Tick};
    use RingSeverity::{Error as Severe, Info, Warn};
    match outcome {
        Outcome::Progress => (0, (RingCode::Progress, Tick, Info)),
        Outcome::Success => (1, (RingCode::Success, End, Info)),
        Outcome::Failure(FailureCode::Validation) => (2, (RingCode::FailureValidation, End, Warn)),
        Outcome::Failure(FailureCode::Unavailable) => (3, (RingCode::FailureUnavailable, End, Severe)),
        Outcome::Failure(FailureCode::Loss) => (4, (RingCode::FailureLoss, End, Severe)),
        Outcome::Failure(FailureCode::Unsupported) => (5, (RingCode::FailureUnsupported, End, Warn)),
        Outcome::Canceled => (6, (RingCode::Canceled, End, Warn)),
    }
}
fn delivery_case(error: DeliveryError) -> (usize, RingKind) {
    use RingPhase::Degraded;
    use RingSeverity::{Error as Severe, Warn};
    match error {
        DeliveryError::Saturated => (0, (RingCode::DeliverySaturated, Degraded, Warn)),
        DeliveryError::Rejected => (1, (RingCode::DeliveryRejected, Degraded, Severe)),
        DeliveryError::Unavailable => (2, (RingCode::DeliveryUnavailable, Degraded, Severe)),
        DeliveryError::Indeterminate => (3, (RingCode::DeliveryIndeterminate, Degraded, Severe)),
    }
}

fn identity() -> RingIdentity {
    RingIdentity {
        module: ModuleId::new(0x0000_0007),
        thread: ThreadRef::new(3),
        session: SessionRef::new([0x11, 0x12]),
        process: ProcessRef::new([0x21, 0x22]),
    }
}
const TIMING: RingTiming = RingTiming { metric_micros: 5, timestamp_nanos: 9 };
fn receipt(outcome: Outcome) -> Receipt {
    Receipt { correlation_id: 42, revision: 7, sequence: 1, outcome, delivered_bytes: 200 }
}
fn every_record() -> Vec<RingRecord> {
    let mut records: Vec<RingRecord> =
        RING_OUTCOMES_ALL.iter().map(|o| RingRecord::terminal(&receipt(*o), identity(), TIMING)).collect();
    records.extend(RING_DELIVERY_ERRORS_ALL.iter().map(|e| {
        RingRecord::delivery_failure(*e, CorrelationRef::new(42), 1, identity(), TIMING)
    }));
    records.extend(RingCode::RESERVED_GPU.iter().map(|c| {
        RingRecord::new((*c, RingPhase::Degraded, RingSeverity::Error), 1, [2, 3], CorrelationRef::new(4), identity(), TIMING)
    }));
    records
}

#[test]
fn ring_record_layout_matches_the_offset_table_and_the_diag_event_core() {
    assert_eq!((size_of::<RingRecord>(), align_of::<RingRecord>()), (104, 8));
    assert_eq!((RING_RECORD_BYTES, RING_CORE_BYTES, host_mirror::DIAG_EVENT_SIZE), (104, 56, 56));
    let o = RING_RECORD_OFFSETS;
    let offsets = [
        o.code,
        o.phase,
        o.severity,
        o.reserved,
        o.thread,
        o.sequence,
        o.counter_a,
        o.counter_b,
        o.metric_micros,
        o.timestamp_nanos,
        o.module,
        o.reserved_tail,
        o.correlation,
        o.session,
        o.process,
    ];
    assert_eq!(offsets, RING_TABLE.map(|(at, _)| at));
    assert_eq!(RING_TABLE[..10], DIAG_EVENT_CORE);
    assert_eq!(o.size, 104);
    // Contiguous fields: every field ends where the next starts and the last ends at the size (no padding).
    for pair in RING_TABLE.windows(2) {
        assert_eq!(pair[0].0 + pair[0].1, pair[1].0);
    }
    assert_eq!(RING_TABLE[14].0 + RING_TABLE[14].1, RING_RECORD_BYTES);
    // Field-by-field little-endian placement with distinct values.
    let id = RingIdentity {
        module: ModuleId::new(0xA1A2_A3A4),
        thread: ThreadRef::new(0xB1B2_B3B4_B5B6_B7B8),
        session: SessionRef::new([0xC1C2_C3C4_C5C6_C7C8, 0xD1D2_D3D4_D5D6_D7D8]),
        process: ProcessRef::new([0xE1E2_E3E4_E5E6_E7E8, 0xF1F2_F3F4_F5F6_F7F8]),
    };
    let timing = RingTiming { metric_micros: 0x5152_5354_5556_5758, timestamp_nanos: 0x6162_6364_6566_6768 };
    let record = RingRecord::new(
        (RingCode::FailureLoss, RingPhase::End, RingSeverity::Error),
        0x7172_7374_7576_7778,
        [0x8182_8384_8586_8788, 0x9192_9394_9596_9798],
        CorrelationRef::new(0x2122_2324_2526_2728),
        id,
        timing,
    );
    let b = record.to_le_bytes();
    let u64_at = |at: usize| u64::from_le_bytes(b[at..at + 8].try_into().unwrap());
    assert_eq!(u16::from_le_bytes([b[0], b[1]]), 0x5304);
    assert_eq!((b[2], b[3]), (host_mirror::PHASE_END, host_mirror::SEVERITY_ERROR));
    assert_eq!(b[4..8], [0; 4]);
    assert_eq!(
        [u64_at(8), u64_at(16), u64_at(24), u64_at(32), u64_at(40), u64_at(48)],
        [
            0xB1B2_B3B4_B5B6_B7B8,
            0x7172_7374_7576_7778,
            0x8182_8384_8586_8788,
            0x9192_9394_9596_9798,
            0x5152_5354_5556_5758,
            0x6162_6364_6566_6768
        ]
    );
    assert_eq!(u32::from_le_bytes(b[56..60].try_into().unwrap()), 0xA1A2_A3A4);
    assert_eq!(b[60..64], [0; 4]);
    assert_eq!(
        [u64_at(64), u64_at(72), u64_at(80), u64_at(88), u64_at(96)],
        [
            0x2122_2324_2526_2728,
            0xC1C2_C3C4_C5C6_C7C8,
            0xD1D2_D3D4_D5D6_D7D8,
            0xE1E2_E3E4_E5E6_E7E8,
            0xF1F2_F3F4_F5F6_F7F8
        ]
    );
    // Typed getters read back the typed parts.
    assert_eq!(
        (record.code(), record.phase(), record.severity(), record.sequence(), record.counters()),
        (
            RingCode::FailureLoss,
            RingPhase::End,
            RingSeverity::Error,
            0x7172_7374_7576_7778,
            [0x8182_8384_8586_8788, 0x9192_9394_9596_9798]
        )
    );
    assert_eq!((record.module(), record.thread(), record.session(), record.process()), (id.module, id.thread, id.session, id.process));
    assert_eq!((record.correlation().get(), record.timing()), (0x2122_2324_2526_2728, timing));
}

#[test]
fn reserved_bytes_are_zero_for_every_constructor() {
    let records = every_record();
    assert_eq!(records.len(), RING_OUTCOMES_ALL.len() + RING_DELIVERY_ERRORS_ALL.len() + 7);
    for record in records {
        let b = record.to_le_bytes();
        assert_eq!(b[4..8], [0; 4], "{record:?}");
        assert_eq!(b[60..64], [0; 4], "{record:?}");
    }
    // Placeholders default to zero ("not supplied"); observe never fills a process identity itself.
    let unsupplied = RingIdentity::default();
    assert_eq!(
        (unsupplied.module.get(), unsupplied.thread.get(), unsupplied.session.get(), unsupplied.process.get()),
        (0, 0, [0, 0], [0, 0])
    );
    assert_eq!((ProcessRef::NOT_SUPPLIED, SessionRef::NOT_SUPPLIED), (ProcessRef::default(), SessionRef::default()));
    let b = RingRecord::terminal(&receipt(Outcome::Success), unsupplied, RingTiming::default()).to_le_bytes();
    assert_eq!(b[56..60], [0; 4]);
    assert_eq!(b[72..104], [0; 32]);
}

#[test]
fn every_outcome_and_delivery_error_maps_to_a_distinct_code() {
    // The ALL arrays are exactly the exhaustive-match cases, in order.
    let outcome_index: Vec<usize> = RING_OUTCOMES_ALL.iter().map(|o| outcome_case(*o).0).collect();
    assert_eq!(outcome_index, (0..7).collect::<Vec<_>>());
    let delivery_index: Vec<usize> = RING_DELIVERY_ERRORS_ALL.iter().map(|e| delivery_case(*e).0).collect();
    assert_eq!(delivery_index, (0..4).collect::<Vec<_>>());
    // Mapping equals the declared Studio readings.
    for outcome in RING_OUTCOMES_ALL {
        assert_eq!(ring_of_outcome(outcome), outcome_case(outcome).1, "{outcome:?}");
    }
    for error in RING_DELIVERY_ERRORS_ALL {
        assert_eq!(ring_of_delivery(error), delivery_case(error).1, "{error:?}");
    }
    // Distinct codes; together with the reserved GPU vocabulary they are RingCode::ALL; no producer path in
    // observe yields a reserved GPU code.
    let mut produced: Vec<RingCode> = RING_OUTCOMES_ALL.iter().map(|o| ring_of_outcome(*o).0).collect();
    produced.extend(RING_DELIVERY_ERRORS_ALL.iter().map(|e| ring_of_delivery(*e).0));
    let mut unique = produced.clone();
    unique.sort_by_key(|c| c.as_u16());
    unique.dedup();
    assert_eq!(unique.len(), 11);
    assert!(produced.iter().all(|c| !RingCode::RESERVED_GPU.contains(c)));
    let mut all = produced;
    all.extend(RingCode::RESERVED_GPU);
    assert_eq!(all, RingCode::ALL.to_vec());
    // Phase and severity discriminants equal the host DiagPhase / DiagSeverity mirror.
    assert_eq!(
        RingPhase::ALL.map(RingPhase::as_u8),
        [
            host_mirror::PHASE_START,
            host_mirror::PHASE_TICK,
            host_mirror::PHASE_END,
            host_mirror::PHASE_RECOVERED,
            host_mirror::PHASE_DEGRADED
        ]
    );
    assert_eq!(
        RingSeverity::ALL.map(RingSeverity::as_u8),
        [host_mirror::SEVERITY_INFO, host_mirror::SEVERITY_WARN, host_mirror::SEVERITY_ERROR]
    );
}

#[test]
fn discriminants_are_pinned_in_the_studio_block_and_never_collide_with_host_codes() {
    // Host mirror = schema.rs @292d7a1c: Heartbeat 0 .. StalledOperation 11, Other 65535; phases 0..4; severities 0..2.
    assert_eq!(host_mirror::EVENT_CODES, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 65535]);
    assert_eq!(host_mirror::OTHER, 65535);
    assert_eq!(RingCode::ALL, PINNED.map(|(c, _)| c));
    assert_eq!((STUDIO_CODE_BLOCK_FIRST, STUDIO_CODE_BLOCK_LAST), (0x5300, 0x53FF));
    for (code, value) in PINNED {
        assert_eq!(code.as_u16(), value, "{code:?}");
        assert_eq!(RingCode::from_u16(value), Some(code));
        assert!((STUDIO_CODE_BLOCK_FIRST..=STUDIO_CODE_BLOCK_LAST).contains(&value));
        assert!(!host_mirror::EVENT_CODES.contains(&value));
        // Projection onto today's host DiagEventCode: Other, the Studio code is lost there.
        assert_eq!(code.host_event_code(), host_mirror::OTHER, "{code:?}");
    }
    // Closed decode: nothing outside the pinned list decodes (whole u16 space).
    let decoded = (0..=u16::MAX).filter(|v| RingCode::from_u16(*v).is_some()).count();
    assert_eq!(decoded, PINNED.len());
    // The record core carries the Studio code itself, never inside a counter.
    let record = RingRecord::terminal(&receipt(Outcome::Success), identity(), TIMING);
    assert_eq!(record.counters(), [200, 7]);
    assert_eq!(u16::from_le_bytes([record.to_le_bytes()[0], record.to_le_bytes()[1]]), 0x5301);
}

// --- No text crosses -----------------------------------------------------------------------------------------

const DECOY_PRIVATE: &str = "PRIVATE-PROJECT-DECOY-zqxj-WOLF";
const DECOY_RESOURCE: &str = "SDOC-019abcde-0000-7000-8000-00000000beef";
const DECOY_ACTOR: [&str; 6] =
    ["decoy-account-qwk", "decoy-principal-vbn", "decoy-owner-hjy", "decoy-owner-principal-mpz", "decoy-space-rtx", "decoy-session-lfc"];
struct CaptureSink {
    frames: Vec<Vec<u8>>,
    forced: Option<DeliveryError>,
}
impl SinkPort for CaptureSink {
    fn try_send(&mut self, _class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        if let Some(error) = self.forced {
            return Err(error);
        }
        self.frames.push(bytes.to_vec());
        Ok(())
    }
}
fn decoy_observe() -> Observe {
    let [a, b, c, d, e, f] = DECOY_ACTOR;
    Observe::new(
        42,
        7,
        DomainId::parse(DECOY_RESOURCE).unwrap(),
        ActorContext::new(a, b, c, d, e, f).unwrap(),
        Budget::new(4, 8192).unwrap(),
    )
}
fn decoy_observation(outcome: Outcome) -> Observation<'static> {
    Observation {
        correlation_id: 42,
        revision: 7,
        outcome,
        progress: (outcome == Outcome::Progress).then(|| Progress::new(1, 2).unwrap()),
        private_project_text: Some(DECOY_PRIVATE),
    }
}
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn no_text_crosses_the_ring_port() {
    let texts: Vec<&str> = [DECOY_PRIVATE, DECOY_RESOURCE].into_iter().chain(DECOY_ACTOR).collect();
    let mut records = Vec::new();
    for outcome in RING_OUTCOMES_ALL {
        let mut observe = decoy_observe();
        let mut sink = CaptureSink { frames: vec![], forced: None };
        let receipt = observe.emit(decoy_observation(outcome), &CancellationToken::default(), &mut sink).unwrap();
        // Counterfactual: the SinkPort frame really carries the decoy attribution text, so the inputs are live.
        assert!(contains(&sink.frames[0], DECOY_ACTOR[0].as_bytes()));
        assert!(contains(&sink.frames[0], DECOY_RESOURCE.as_bytes()));
        records.push(RingRecord::terminal(&receipt, identity(), TIMING));
    }
    for error in RING_DELIVERY_ERRORS_ALL {
        let mut observe = decoy_observe();
        let mut sink = CaptureSink { frames: vec![], forced: Some(error) };
        let refused = observe.emit(decoy_observation(Outcome::Success), &CancellationToken::default(), &mut sink);
        assert_eq!(refused, Err(Error::Delivery(error)));
        let attempted = observe.state().acknowledged + 1;
        records.push(RingRecord::delivery_failure(error, CorrelationRef::new(42), attempted, identity(), TIMING));
    }
    assert_eq!(records.len(), 11);
    for record in &records {
        let bytes = record.to_le_bytes();
        for text in &texts {
            assert!(!contains(&bytes, text.as_bytes()), "{text} in {record:?}");
            for run in text.as_bytes().windows(4) {
                assert!(!contains(&bytes, run), "4-byte run {:?} of {text} in {record:?}", String::from_utf8_lossy(run));
            }
        }
    }
}

// --- Port contract -------------------------------------------------------------------------------------------

/// Test-local fixed-array producer (never a type in observe): preallocated slots, a drop counter, no lock.
struct FixedRing {
    slots: [RingRecord; 4],
    len: usize,
    dropped: u64,
}
impl RingProducerPort for FixedRing {
    fn try_record(&mut self, record: &RingRecord) -> Result<(), RingRefusal> {
        match self.slots.get_mut(self.len) {
            Some(slot) => {
                *slot = *record;
                self.len += 1;
                Ok(())
            }
            None => {
                self.dropped += 1;
                Err(RingRefusal::Saturated)
            }
        }
    }
}
struct NoRing;
impl RingProducerPort for NoRing {
    fn try_record(&mut self, _record: &RingRecord) -> Result<(), RingRefusal> {
        Err(RingRefusal::Unavailable)
    }
}
fn is_copy<T: Copy>() {}
fn is_send<T: Send>() {}

#[test]
fn producer_records_from_a_second_thread_and_saturation_is_typed() {
    is_copy::<RingRecord>();
    is_copy::<Result<(), RingRefusal>>();
    is_send::<FixedRing>();
    let blank = RingRecord::new(ring_of_outcome(Outcome::Progress), 0, [0, 0], CorrelationRef::NOT_SUPPLIED, RingIdentity::default(), RingTiming::default());
    let mut ring = FixedRing { slots: [blank; 4], len: 0, dropped: 0 };
    let (results, allocations) = std::thread::scope(|scope| {
        let ring = &mut ring;
        scope
            .spawn(move || {
                allocations_during(|| {
                    let port: &mut dyn RingProducerPort = ring;
                    let mut results = [Ok(()); 6];
                    for (i, result) in results.iter_mut().enumerate() {
                        let outcome = RING_OUTCOMES_ALL[i % RING_OUTCOMES_ALL.len()];
                        let record = if i % 2 == 0 {
                            RingRecord::terminal(&receipt(outcome), identity(), TIMING)
                        } else {
                            RingRecord::delivery_failure(DeliveryError::Saturated, CorrelationRef::new(42), 2, identity(), TIMING)
                        };
                        *result = port.try_record(&record);
                    }
                    results
                })
            })
            .join()
            .unwrap()
    });
    assert_eq!(allocations, 0, "record construction and try_record must not allocate");
    assert_eq!(results, [Ok(()), Ok(()), Ok(()), Ok(()), Err(RingRefusal::Saturated), Err(RingRefusal::Saturated)]);
    assert_eq!((ring.len, ring.dropped), (4, 2));
    assert_eq!(ring.slots[0].code(), RingCode::Progress);
    assert_eq!(ring.slots[1].code(), RingCode::DeliverySaturated);
    // The counting allocator is live (counterfactual): a Vec allocation on an armed thread is counted.
    let (_, counted) = allocations_during(|| Vec::<u8>::with_capacity(16));
    assert_eq!(counted, 1);
    assert_eq!(NoRing.try_record(&blank), Err(RingRefusal::Unavailable));
}

#[test]
fn terminal_and_delivery_failure_constructors_carry_counters_per_table() {
    for outcome in RING_OUTCOMES_ALL {
        let receipt = Receipt { correlation_id: 0x99, revision: 0x77, sequence: 0x55, outcome, delivered_bytes: 321 };
        let record = RingRecord::terminal(&receipt, identity(), TIMING);
        assert_eq!((record.code(), record.phase(), record.severity()), ring_of_outcome(outcome));
        assert_eq!((record.sequence(), record.counters(), record.correlation().get()), (0x55, [321, 0x77], 0x99));
        assert_eq!((record.module(), record.thread(), record.session(), record.process(), record.timing()), (identity().module, identity().thread, identity().session, identity().process, TIMING));
    }
    let huge = Receipt { delivered_bytes: usize::MAX, ..receipt(Outcome::Success) };
    assert_eq!(RingRecord::terminal(&huge, identity(), TIMING).counters()[0], u64::try_from(usize::MAX).unwrap_or(u64::MAX));
    for error in RING_DELIVERY_ERRORS_ALL {
        let record = RingRecord::delivery_failure(error, CorrelationRef::new(0x31), 0x41, identity(), TIMING);
        assert_eq!((record.code(), record.phase(), record.severity()), ring_of_delivery(error));
        assert_eq!((record.sequence(), record.counters(), record.correlation().get()), (0x41, [0, 0], 0x31));
        assert_eq!(record.process(), identity().process);
    }
}

#[test]
fn descriptor_declares_the_ring_port() {
    assert_eq!(DESCRIPTOR.matches("\"ring_port\":{").count(), 1);
    let ring = &DESCRIPTOR[DESCRIPTOR.find("\"ring_port\":{").unwrap()..];
    for needle in [
        "not a recorder",
        "104",
        "0x5300..=0x53FF",
        "PROPOSED",
        "Other",
        "RingProducerPort",
        "try_record",
        "MT-36233",
        "SD-04",
        "GUI-DESKTOP-050",
        "tests/ring_port.rs",
    ] {
        assert!(ring.contains(needle), "ring_port lacks {needle}");
    }
    assert!(DESCRIPTOR.ends_with("}}"));
}

/// R46 F-R46-6 (lane SMB-R46-FOLLOWUPS-1): the host write is `host_core_le_bytes`: for every record (all outcome,
/// delivery and reserved GPU codes) bytes 0..2 are the record code's `host_event_code`, a `DiagEventCode` of the
/// schema.rs @292d7a1c table (`host_mirror::EVENT_CODES`; today `Other` for every Studio code), and bytes 2..56
/// equal the record's own core; `to_le_bytes` keeps the Studio code (never an allowlisted host code).
#[test]
fn host_core_carries_the_host_event_code_and_the_record_core() {
    let records = every_record();
    assert_eq!(records.len(), RingCode::ALL.len());
    for record in records {
        let (core, full) = (record.host_core_le_bytes(), record.to_le_bytes());
        let host = u16::from_le_bytes([core[0], core[1]]);
        assert_eq!(host, record.code().host_event_code());
        assert!(host_mirror::EVENT_CODES.contains(&host), "{host} not a DiagEventCode");
        assert_eq!(host, host_mirror::OTHER, "no Studio code allocated at 292d7a1c");
        assert_eq!(core[2..], full[2..RING_CORE_BYTES]);
        let studio = u16::from_le_bytes([full[0], full[1]]);
        assert_eq!(studio, record.code().as_u16());
        assert!(!host_mirror::EVENT_CODES.contains(&studio), "to_le_bytes keeps the Studio code");
    }
}
