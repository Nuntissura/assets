//! Ring-shaped, text-free producer port (RF-04 phase 0, SMB-OBSERVE-RING-PORT-1), beside [`crate::SinkPort`].
//!
//! Observe stays a bounded adapter, not a recorder or authority: this module declares the record shape, the
//! closed code vocabulary, the outcome mapping and the producer port. It has no ring, no reader, no storage, no
//! catalogue and no session registry. The ring, its reader and Palmistry are Handshake's (MT-36233); the host
//! adapter onto `DiagRingWriter` is Handshake work (MT-36190). `Observe::emit*` is not wired to this port in
//! phase 0, and `SinkPort` and the wire versions 1-5 are unchanged.
//!
//! # Record layout ([`RingRecord`], `#[repr(C)]`, 104 bytes, align 8, no implicit padding)
//!
//! The first 56 bytes have the order, widths and offsets of Handshake's `DiagEvent`
//! (`src/frontend/handshake_native/diag_ring/src/schema.rs` @292d7a1c). The tail carries the identity
//! `DiagEvent` lacks today (module, correlation, session, process); Handshake must add it on its side (MT-36233,
//! spec delta SD-04). Every field is an integer, an integer array, a `#[repr(u16)]`/`#[repr(u8)]` closed enum or a
//! `#[repr(transparent)]` integer newtype. Pinned by `tests/ring_port.rs`
//! `ring_record_layout_matches_the_offset_table_and_the_diag_event_core`.
//!
//! ```text
//! offset size field            DiagEvent field
//!      0    2 code (RingCode)  event_code
//!      2    1 phase            phase_marker
//!      3    1 severity         severity
//!      4    4 reserved (zero)  _reserved
//!      8    8 thread           thread_id
//!     16    8 sequence         sequence_id
//!     24    8 counter_a        counter_a
//!     32    8 counter_b        counter_b
//!     40    8 metric_micros    metric_micros
//!     48    8 timestamp_nanos  timestamp_nanos
//!     56    4 module (ModuleId)          -- identity tail, not in DiagEvent at 292d7a1c
//!     60    4 reserved_tail (zero)
//!     64    8 correlation (CorrelationRef)
//!     72   16 session (SessionRef, [u64; 2])
//!     88   16 process (ProcessRef, [u64; 2], PID + process birth, host only)
//! total 104
//! ```
//!
//! # Codes ([`RingCode`], PROPOSED Studio block 0x5300..=0x53FF)
//!
//! The block is a proposal to Handshake, not an allocation: Handshake's `DiagEventCode` uses 0..=11 and 65535
//! and has no Studio or GUI code at 292d7a1c. Until Handshake allocates the block, the host projection
//! ([`RingCode::host_event_code`]) is `Other` (65535) for every Studio code and the Studio code is lost in the
//! 56-byte core; it survives only in this record's `code` field. The code is never put into `counter_a` or
//! `counter_b`, whose meaning is per code. [`RingRecord::to_le_bytes`] carries the Studio code in bytes 0..2;
//! the host write is [`RingRecord::host_core_le_bytes`], the 56-byte core with the projected `event_code`.
//!
//! | code | value | phase | severity | source |
//! |---|---|---|---|---|
//! | Progress | 0x5300 | Tick | Info | `Outcome::Progress` |
//! | Success | 0x5301 | End | Info | `Outcome::Success` |
//! | FailureValidation | 0x5302 | End | Warn | `Outcome::Failure(Validation)` |
//! | FailureUnavailable | 0x5303 | End | Error | `Outcome::Failure(Unavailable)` |
//! | FailureLoss | 0x5304 | End | Error | `Outcome::Failure(Loss)` |
//! | FailureUnsupported | 0x5305 | End | Warn | `Outcome::Failure(Unsupported)` |
//! | Canceled | 0x5306 | End | Warn | `Outcome::Canceled` |
//! | DeliverySaturated | 0x5310 | Degraded | Warn | `DeliveryError::Saturated` |
//! | DeliveryRejected | 0x5311 | Degraded | Error | `DeliveryError::Rejected` |
//! | DeliveryUnavailable | 0x5312 | Degraded | Error | `DeliveryError::Unavailable` |
//! | DeliveryIndeterminate | 0x5313 | Degraded | Error | `DeliveryError::Indeterminate` |
//! | GpuUnavailable .. GpuRebound | 0x5320..=0x5326 | caller | caller | reserved, see below |
//!
//! Phase and severity are Studio readings of the host's `DiagPhase`/`DiagSeverity` (same discriminants).
//! The seven `Gpu*` codes are declared vocabulary for the GPU client outcomes that SMB-GPU-CLIENT-PORT-1 types
//! (render-gpu); no observe producer emits them yet ([`ring_of_outcome`] and [`ring_of_delivery`] never return
//! them, pinned by `every_outcome_and_delivery_error_maps_to_a_distinct_code`), so that follow-up needs no second
//! observe change.
//!
//! # Counters per code
//!
//! | code | sequence | counter_a | counter_b |
//! |---|---|---|---|
//! | outcome codes ([`RingRecord::terminal`]) | receipt sequence | delivered frame bytes | receipt revision |
//! | delivery codes ([`RingRecord::delivery_failure`]) | attempted sequence | 0 | 0 |
//! | Gpu* (reserved) | caller | caller | caller |
//!
//! `metric_micros` and `timestamp_nanos` come from the caller's [`RingTiming`]; observe reads no clock.
//!
//! # Identity
//!
//! Module, thread, session and process are opaque integers supplied by the caller ([`RingIdentity`]); zero means
//! "not supplied". Observe never derives an id from text (no hashing of a `DomainId`, account or session string,
//! which would be dictionary-reversible) and never invents a process identity (GUI-DESKTOP-050): the process id
//! and birth are filled only by the host. The correlation is observe's existing u64 correlation id; it is not a
//! host acceptance identity and grants nothing.
//!
//! # No text
//!
//! No public type of this module holds a `String`, `&str`, `Vec` or slice, and nothing converts from text:
//!
//! ```compile_fail,E0277
//! use hsk_studio_observe::ring::RingRecord;
//! let _record = RingRecord::from("private project text");
//! ```
//!
//! ```compile_fail,E0277
//! use hsk_studio_observe::ring::ModuleId;
//! let _module = ModuleId::from("x");
//! ```
//!
//! A record is built only from typed parts:
//!
//! ```
//! use hsk_studio_observe::ring::{
//!     ring_of_outcome, CorrelationRef, ModuleId, RingCode, RingIdentity, RingRecord, RingTiming,
//! };
//! use hsk_studio_observe::Outcome;
//! let identity = RingIdentity { module: ModuleId::new(7), ..RingIdentity::default() };
//! let timing = RingTiming { metric_micros: 0, timestamp_nanos: 1_000 };
//! let record = RingRecord::new(ring_of_outcome(Outcome::Success), 1, [0, 0], CorrelationRef::new(42), identity, timing);
//! assert_eq!(record.code(), RingCode::Success);
//! assert_eq!(record.to_le_bytes()[56..60], 7u32.to_le_bytes());
//! ```
#![forbid(unsafe_code)]
#![deny(clippy::arithmetic_side_effects)]

use crate::{DeliveryError, FailureCode, Outcome, Receipt};

/// Size of a [`RingRecord`] in bytes (56-byte `DiagEvent` core plus the 48-byte identity tail).
pub const RING_RECORD_BYTES: usize = 104;
/// Size of the core that matches Handshake's `DiagEvent` (`DIAG_EVENT_SIZE` @292d7a1c).
pub const RING_CORE_BYTES: usize = 56;
/// First code of the PROPOSED Studio block (not allocated by Handshake).
pub const STUDIO_CODE_BLOCK_FIRST: u16 = 0x5300;
/// Last code of the PROPOSED Studio block.
pub const STUDIO_CODE_BLOCK_LAST: u16 = 0x53FF;

/// Contract mirror of Handshake `src/frontend/handshake_native/diag_ring/src/schema.rs` @292d7a1c
/// (`DiagEventCode`, `DiagPhase`, `DiagSeverity`, `DIAG_EVENT_SIZE`). Read-only copy for the projection table
/// and its test; not a dependency and not an edit of the host contract.
pub mod host_mirror {
    pub const DIAG_EVENT_SIZE: usize = 56;
    pub const HEARTBEAT: u16 = 0;
    pub const PANIC_CAUGHT: u16 = 1;
    pub const SLOW_FRAME: u16 = 2;
    pub const RESOURCE_SAMPLE: u16 = 3;
    pub const BACKEND_UNREACHABLE: u16 = 4;
    pub const BACKEND_RECOVERED: u16 = 5;
    pub const PANE_MOUNTED: u16 = 6;
    pub const FREEZE_SUSPECTED: u16 = 7;
    pub const CRASH_DETECTED: u16 = 8;
    pub const PALMISTRY_HANDSHAKE: u16 = 9;
    pub const SHUTDOWN: u16 = 10;
    pub const STALLED_OPERATION: u16 = 11;
    pub const OTHER: u16 = 65535;
    /// Every `DiagEventCode` discriminant at 292d7a1c.
    pub const EVENT_CODES: [u16; 13] = [
        HEARTBEAT,
        PANIC_CAUGHT,
        SLOW_FRAME,
        RESOURCE_SAMPLE,
        BACKEND_UNREACHABLE,
        BACKEND_RECOVERED,
        PANE_MOUNTED,
        FREEZE_SUSPECTED,
        CRASH_DETECTED,
        PALMISTRY_HANDSHAKE,
        SHUTDOWN,
        STALLED_OPERATION,
        OTHER,
    ];
    pub const PHASE_START: u8 = 0;
    pub const PHASE_TICK: u8 = 1;
    pub const PHASE_END: u8 = 2;
    pub const PHASE_RECOVERED: u8 = 3;
    pub const PHASE_DEGRADED: u8 = 4;
    pub const SEVERITY_INFO: u8 = 0;
    pub const SEVERITY_WARN: u8 = 1;
    pub const SEVERITY_ERROR: u8 = 2;
}

/// Closed Studio ring vocabulary; explicit discriminants in the PROPOSED block 0x5300..=0x53FF.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum RingCode {
    Progress = 0x5300,
    Success = 0x5301,
    FailureValidation = 0x5302,
    FailureUnavailable = 0x5303,
    FailureLoss = 0x5304,
    FailureUnsupported = 0x5305,
    Canceled = 0x5306,
    DeliverySaturated = 0x5310,
    DeliveryRejected = 0x5311,
    DeliveryUnavailable = 0x5312,
    DeliveryIndeterminate = 0x5313,
    /// Reserved for render-gpu client outcomes (SMB-GPU-CLIENT-PORT-1); no observe producer emits it.
    GpuUnavailable = 0x5320,
    /// Reserved (render-gpu); no observe producer emits it.
    GpuDeviceLost = 0x5321,
    /// Reserved (render-gpu); no observe producer emits it.
    GpuAdmissionDenied = 0x5322,
    /// Reserved (render-gpu); no observe producer emits it.
    GpuBudgetExceeded = 0x5323,
    /// Reserved (render-gpu); no observe producer emits it.
    GpuDoubleRelease = 0x5324,
    /// Reserved (render-gpu); no observe producer emits it.
    GpuLateRelease = 0x5325,
    /// Reserved (render-gpu); no observe producer emits it.
    GpuRebound = 0x5326,
}
impl RingCode {
    /// Every code, in discriminant order.
    pub const ALL: [RingCode; 18] = [
        Self::Progress,
        Self::Success,
        Self::FailureValidation,
        Self::FailureUnavailable,
        Self::FailureLoss,
        Self::FailureUnsupported,
        Self::Canceled,
        Self::DeliverySaturated,
        Self::DeliveryRejected,
        Self::DeliveryUnavailable,
        Self::DeliveryIndeterminate,
        Self::GpuUnavailable,
        Self::GpuDeviceLost,
        Self::GpuAdmissionDenied,
        Self::GpuBudgetExceeded,
        Self::GpuDoubleRelease,
        Self::GpuLateRelease,
        Self::GpuRebound,
    ];
    /// The declared GPU client vocabulary (no observe producer yet).
    pub const RESERVED_GPU: [RingCode; 7] = [
        Self::GpuUnavailable,
        Self::GpuDeviceLost,
        Self::GpuAdmissionDenied,
        Self::GpuBudgetExceeded,
        Self::GpuDoubleRelease,
        Self::GpuLateRelease,
        Self::GpuRebound,
    ];
    pub const fn as_u16(self) -> u16 {
        self as u16
    }
    /// Closed decode; any value outside [`RingCode::ALL`] is `None`.
    pub const fn from_u16(raw: u16) -> Option<Self> {
        Some(match raw {
            0x5300 => Self::Progress,
            0x5301 => Self::Success,
            0x5302 => Self::FailureValidation,
            0x5303 => Self::FailureUnavailable,
            0x5304 => Self::FailureLoss,
            0x5305 => Self::FailureUnsupported,
            0x5306 => Self::Canceled,
            0x5310 => Self::DeliverySaturated,
            0x5311 => Self::DeliveryRejected,
            0x5312 => Self::DeliveryUnavailable,
            0x5313 => Self::DeliveryIndeterminate,
            0x5320 => Self::GpuUnavailable,
            0x5321 => Self::GpuDeviceLost,
            0x5322 => Self::GpuAdmissionDenied,
            0x5323 => Self::GpuBudgetExceeded,
            0x5324 => Self::GpuDoubleRelease,
            0x5325 => Self::GpuLateRelease,
            0x5326 => Self::GpuRebound,
            _ => return None,
        })
    }
    /// Projection onto today's host `DiagEventCode` (@292d7a1c): `Other` for every Studio code, because the
    /// host has allocated no Studio or GUI code. The Studio code is lost in that projection. Exhaustive (no
    /// wildcard) so a new code must choose its projection.
    pub const fn host_event_code(self) -> u16 {
        match self {
            Self::Progress
            | Self::Success
            | Self::FailureValidation
            | Self::FailureUnavailable
            | Self::FailureLoss
            | Self::FailureUnsupported
            | Self::Canceled
            | Self::DeliverySaturated
            | Self::DeliveryRejected
            | Self::DeliveryUnavailable
            | Self::DeliveryIndeterminate
            | Self::GpuUnavailable
            | Self::GpuDeviceLost
            | Self::GpuAdmissionDenied
            | Self::GpuBudgetExceeded
            | Self::GpuDoubleRelease
            | Self::GpuLateRelease
            | Self::GpuRebound => host_mirror::OTHER,
        }
    }
}

/// Same discriminants as the host's `DiagPhase`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RingPhase {
    Start = 0,
    Tick = 1,
    End = 2,
    Recovered = 3,
    Degraded = 4,
}
impl RingPhase {
    pub const ALL: [RingPhase; 5] = [Self::Start, Self::Tick, Self::End, Self::Recovered, Self::Degraded];
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Same discriminants as the host's `DiagSeverity`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RingSeverity {
    Info = 0,
    Warn = 1,
    Error = 2,
}
impl RingSeverity {
    pub const ALL: [RingSeverity; 3] = [Self::Info, Self::Warn, Self::Error];
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Code, phase and severity of one record.
pub type RingKind = (RingCode, RingPhase, RingSeverity);

/// Opaque module id assigned by the host registration; 0 = not supplied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ModuleId(u32);
impl ModuleId {
    pub const NOT_SUPPLIED: Self = Self(0);
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }
    pub const fn get(self) -> u32 {
        self.0
    }
}
/// Opaque producer thread number; 0 = not supplied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ThreadRef(u64);
impl ThreadRef {
    pub const NOT_SUPPLIED: Self = Self(0);
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}
/// Observe's u64 correlation id; not a host acceptance identity and grants nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct CorrelationRef(u64);
impl CorrelationRef {
    pub const NOT_SUPPLIED: Self = Self(0);
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}
/// Opaque 128-bit session placeholder, filled by the host; zero = not supplied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct SessionRef([u64; 2]);
impl SessionRef {
    pub const NOT_SUPPLIED: Self = Self([0; 2]);
    pub const fn new(raw: [u64; 2]) -> Self {
        Self(raw)
    }
    pub const fn get(self) -> [u64; 2] {
        self.0
    }
}
/// Opaque 128-bit process placeholder (PID plus process birth), filled only by the host; zero = not supplied.
/// Observe never invents a process identity (GUI-DESKTOP-050).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ProcessRef([u64; 2]);
impl ProcessRef {
    pub const NOT_SUPPLIED: Self = Self([0; 2]);
    pub const fn new(raw: [u64; 2]) -> Self {
        Self(raw)
    }
    pub const fn get(self) -> [u64; 2] {
        self.0
    }
}

/// Producer identity supplied by the caller; `Default` is "not supplied" everywhere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RingIdentity {
    pub module: ModuleId,
    pub thread: ThreadRef,
    pub session: SessionRef,
    pub process: ProcessRef,
}
/// Caller-supplied timing; observe reads no clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RingTiming {
    pub metric_micros: u64,
    pub timestamp_nanos: u64,
}

/// Fixed-size, text-free ring record (layout in the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct RingRecord {
    code: RingCode,
    phase: RingPhase,
    severity: RingSeverity,
    reserved: [u8; 4],
    thread: ThreadRef,
    sequence: u64,
    counter_a: u64,
    counter_b: u64,
    metric_micros: u64,
    timestamp_nanos: u64,
    module: ModuleId,
    reserved_tail: u32,
    correlation: CorrelationRef,
    session: SessionRef,
    process: ProcessRef,
}

/// Byte offset of every [`RingRecord`] field, computed with `offset_of!` and pinned below.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingRecordOffsets {
    pub code: usize,
    pub phase: usize,
    pub severity: usize,
    pub reserved: usize,
    pub thread: usize,
    pub sequence: usize,
    pub counter_a: usize,
    pub counter_b: usize,
    pub metric_micros: usize,
    pub timestamp_nanos: usize,
    pub module: usize,
    pub reserved_tail: usize,
    pub correlation: usize,
    pub session: usize,
    pub process: usize,
    pub size: usize,
}
pub const RING_RECORD_OFFSETS: RingRecordOffsets = RingRecordOffsets {
    code: core::mem::offset_of!(RingRecord, code),
    phase: core::mem::offset_of!(RingRecord, phase),
    severity: core::mem::offset_of!(RingRecord, severity),
    reserved: core::mem::offset_of!(RingRecord, reserved),
    thread: core::mem::offset_of!(RingRecord, thread),
    sequence: core::mem::offset_of!(RingRecord, sequence),
    counter_a: core::mem::offset_of!(RingRecord, counter_a),
    counter_b: core::mem::offset_of!(RingRecord, counter_b),
    metric_micros: core::mem::offset_of!(RingRecord, metric_micros),
    timestamp_nanos: core::mem::offset_of!(RingRecord, timestamp_nanos),
    module: core::mem::offset_of!(RingRecord, module),
    reserved_tail: core::mem::offset_of!(RingRecord, reserved_tail),
    correlation: core::mem::offset_of!(RingRecord, correlation),
    session: core::mem::offset_of!(RingRecord, session),
    process: core::mem::offset_of!(RingRecord, process),
    size: core::mem::size_of::<RingRecord>(),
};
const OFF_CODE: usize = 0;
const OFF_PHASE: usize = 2;
const OFF_SEVERITY: usize = 3;
const OFF_RESERVED: usize = 4;
const OFF_THREAD: usize = 8;
const OFF_SEQUENCE: usize = 16;
const OFF_COUNTER_A: usize = 24;
const OFF_COUNTER_B: usize = 32;
const OFF_METRIC: usize = 40;
const OFF_TIMESTAMP: usize = 48;
const OFF_MODULE: usize = 56;
const OFF_RESERVED_TAIL: usize = 60;
const OFF_CORRELATION: usize = 64;
const OFF_SESSION: usize = 72;
const OFF_SESSION_HI: usize = 80;
const OFF_PROCESS: usize = 88;
const OFF_PROCESS_HI: usize = 96;
const _: () = {
    let o = RING_RECORD_OFFSETS;
    assert!(o.code == OFF_CODE && o.phase == OFF_PHASE && o.severity == OFF_SEVERITY);
    assert!(o.reserved == OFF_RESERVED && o.thread == OFF_THREAD && o.sequence == OFF_SEQUENCE);
    assert!(o.counter_a == OFF_COUNTER_A && o.counter_b == OFF_COUNTER_B && o.metric_micros == OFF_METRIC);
    assert!(o.timestamp_nanos == OFF_TIMESTAMP && o.module == OFF_MODULE && o.reserved_tail == OFF_RESERVED_TAIL);
    assert!(o.correlation == OFF_CORRELATION && o.session == OFF_SESSION && o.process == OFF_PROCESS);
    assert!(OFF_MODULE == RING_CORE_BYTES && RING_CORE_BYTES == host_mirror::DIAG_EVENT_SIZE);
    assert!(o.size == RING_RECORD_BYTES && core::mem::align_of::<RingRecord>() == 8);
    // Sum of field sizes equals the record size: no implicit padding anywhere.
    assert!(2 + 1 + 1 + 4 + 6 * 8 + 4 + 4 + 8 + 16 + 16 == RING_RECORD_BYTES);
    assert!(core::mem::size_of::<RingCode>() == 2 && core::mem::size_of::<RingPhase>() == 1);
    assert!(core::mem::size_of::<RingSeverity>() == 1 && core::mem::size_of::<ModuleId>() == 4);
    assert!(core::mem::size_of::<SessionRef>() == 16 && core::mem::size_of::<ProcessRef>() == 16);
};

impl RingRecord {
    /// The one typed constructor; both reserved fields are zero.
    pub const fn new(
        kind: RingKind,
        sequence: u64,
        counters: [u64; 2],
        correlation: CorrelationRef,
        identity: RingIdentity,
        timing: RingTiming,
    ) -> Self {
        Self {
            code: kind.0,
            phase: kind.1,
            severity: kind.2,
            reserved: [0; 4],
            thread: identity.thread,
            sequence,
            counter_a: counters[0],
            counter_b: counters[1],
            metric_micros: timing.metric_micros,
            timestamp_nanos: timing.timestamp_nanos,
            module: identity.module,
            reserved_tail: 0,
            correlation,
            session: identity.session,
            process: identity.process,
        }
    }
    /// Record of an accepted emission: code from the receipt outcome, sequence from the receipt,
    /// `counter_a` = delivered frame bytes (saturating), `counter_b` = receipt revision, correlation from the
    /// receipt. A progress receipt maps to `Progress`/`Tick`.
    pub const fn terminal(receipt: &Receipt, identity: RingIdentity, timing: RingTiming) -> Self {
        let delivered = if receipt.delivered_bytes as u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            receipt.delivered_bytes as u64
        };
        Self::new(
            ring_of_outcome(receipt.outcome),
            receipt.sequence,
            [delivered, receipt.revision],
            CorrelationRef::new(receipt.correlation_id),
            identity,
            timing,
        )
    }
    /// Record of a refused delivery: code from the error, `sequence` = the attempted sequence (the emitter's
    /// acknowledged sequence + 1), both counters zero.
    pub const fn delivery_failure(
        error: DeliveryError,
        correlation: CorrelationRef,
        attempted_sequence: u64,
        identity: RingIdentity,
        timing: RingTiming,
    ) -> Self {
        Self::new(ring_of_delivery(error), attempted_sequence, [0, 0], correlation, identity, timing)
    }
    pub const fn code(&self) -> RingCode {
        self.code
    }
    pub const fn phase(&self) -> RingPhase {
        self.phase
    }
    pub const fn severity(&self) -> RingSeverity {
        self.severity
    }
    pub const fn thread(&self) -> ThreadRef {
        self.thread
    }
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
    pub const fn counters(&self) -> [u64; 2] {
        [self.counter_a, self.counter_b]
    }
    pub const fn timing(&self) -> RingTiming {
        RingTiming {
            metric_micros: self.metric_micros,
            timestamp_nanos: self.timestamp_nanos,
        }
    }
    pub const fn module(&self) -> ModuleId {
        self.module
    }
    pub const fn correlation(&self) -> CorrelationRef {
        self.correlation
    }
    pub const fn session(&self) -> SessionRef {
        self.session
    }
    pub const fn process(&self) -> ProcessRef {
        self.process
    }
    /// Field-by-field little-endian bytes in the layout of the module docs (for the host adapter and tests).
    pub fn to_le_bytes(&self) -> [u8; RING_RECORD_BYTES] {
        let mut out = [0u8; RING_RECORD_BYTES];
        out[OFF_CODE..OFF_PHASE].copy_from_slice(&self.code.as_u16().to_le_bytes());
        out[OFF_PHASE] = self.phase.as_u8();
        out[OFF_SEVERITY] = self.severity.as_u8();
        out[OFF_RESERVED..OFF_THREAD].copy_from_slice(&self.reserved);
        out[OFF_THREAD..OFF_SEQUENCE].copy_from_slice(&self.thread.get().to_le_bytes());
        out[OFF_SEQUENCE..OFF_COUNTER_A].copy_from_slice(&self.sequence.to_le_bytes());
        out[OFF_COUNTER_A..OFF_COUNTER_B].copy_from_slice(&self.counter_a.to_le_bytes());
        out[OFF_COUNTER_B..OFF_METRIC].copy_from_slice(&self.counter_b.to_le_bytes());
        out[OFF_METRIC..OFF_TIMESTAMP].copy_from_slice(&self.metric_micros.to_le_bytes());
        out[OFF_TIMESTAMP..OFF_MODULE].copy_from_slice(&self.timestamp_nanos.to_le_bytes());
        out[OFF_MODULE..OFF_RESERVED_TAIL].copy_from_slice(&self.module.get().to_le_bytes());
        out[OFF_RESERVED_TAIL..OFF_CORRELATION].copy_from_slice(&self.reserved_tail.to_le_bytes());
        out[OFF_CORRELATION..OFF_SESSION].copy_from_slice(&self.correlation.get().to_le_bytes());
        let [session_lo, session_hi] = self.session.get();
        out[OFF_SESSION..OFF_SESSION_HI].copy_from_slice(&session_lo.to_le_bytes());
        out[OFF_SESSION_HI..OFF_PROCESS].copy_from_slice(&session_hi.to_le_bytes());
        let [process_lo, process_hi] = self.process.get();
        out[OFF_PROCESS..OFF_PROCESS_HI].copy_from_slice(&process_lo.to_le_bytes());
        out[OFF_PROCESS_HI..RING_RECORD_BYTES].copy_from_slice(&process_hi.to_le_bytes());
        out
    }
    /// The 56-byte `DiagEvent` core as the host writes it (R46 F-R46-6, lane SMB-OBSERVE-RING-PORT-1 follow-up
    /// SMB-R46-FOLLOWUPS-1): bytes 0..2 carry [`RingCode::host_event_code`] (a `DiagEventCode` of
    /// [`host_mirror::EVENT_CODES`], today `Other` for every Studio code), bytes 2..56 equal
    /// [`RingRecord::to_le_bytes`]. [`RingRecord::to_le_bytes`] carries the Studio code in bytes 0..2 and is not a
    /// host write: an adapter onto `DiagRingWriter` copies this core, never the first 56 bytes of the record.
    /// Pinned by `tests/ring_port.rs` `host_core_carries_the_host_event_code_and_the_record_core`.
    pub fn host_core_le_bytes(&self) -> [u8; RING_CORE_BYTES] {
        let full = self.to_le_bytes();
        let mut out = [0u8; RING_CORE_BYTES];
        out.copy_from_slice(&full[..RING_CORE_BYTES]);
        out[OFF_CODE..OFF_PHASE].copy_from_slice(&self.code.host_event_code().to_le_bytes());
        out
    }
}

/// Every `Outcome` value (all four `FailureCode`s), in [`RingCode`] order.
pub const RING_OUTCOMES_ALL: [Outcome; 7] = [
    Outcome::Progress,
    Outcome::Success,
    Outcome::Failure(FailureCode::Validation),
    Outcome::Failure(FailureCode::Unavailable),
    Outcome::Failure(FailureCode::Loss),
    Outcome::Failure(FailureCode::Unsupported),
    Outcome::Canceled,
];
/// Every `DeliveryError` value, in [`RingCode`] order.
pub const RING_DELIVERY_ERRORS_ALL: [DeliveryError; 4] = [
    DeliveryError::Saturated,
    DeliveryError::Rejected,
    DeliveryError::Unavailable,
    DeliveryError::Indeterminate,
];

/// Studio reading of an outcome (table in the module docs). Exhaustive, no wildcard.
pub const fn ring_of_outcome(outcome: Outcome) -> RingKind {
    match outcome {
        Outcome::Progress => (RingCode::Progress, RingPhase::Tick, RingSeverity::Info),
        Outcome::Success => (RingCode::Success, RingPhase::End, RingSeverity::Info),
        Outcome::Failure(FailureCode::Validation) => {
            (RingCode::FailureValidation, RingPhase::End, RingSeverity::Warn)
        }
        Outcome::Failure(FailureCode::Unavailable) => {
            (RingCode::FailureUnavailable, RingPhase::End, RingSeverity::Error)
        }
        Outcome::Failure(FailureCode::Loss) => (RingCode::FailureLoss, RingPhase::End, RingSeverity::Error),
        Outcome::Failure(FailureCode::Unsupported) => {
            (RingCode::FailureUnsupported, RingPhase::End, RingSeverity::Warn)
        }
        Outcome::Canceled => (RingCode::Canceled, RingPhase::End, RingSeverity::Warn),
    }
}
/// Studio reading of a delivery refusal (table in the module docs). Exhaustive, no wildcard.
pub const fn ring_of_delivery(error: DeliveryError) -> RingKind {
    match error {
        DeliveryError::Saturated => (RingCode::DeliverySaturated, RingPhase::Degraded, RingSeverity::Warn),
        DeliveryError::Rejected => (RingCode::DeliveryRejected, RingPhase::Degraded, RingSeverity::Error),
        DeliveryError::Unavailable => {
            (RingCode::DeliveryUnavailable, RingPhase::Degraded, RingSeverity::Error)
        }
        DeliveryError::Indeterminate => {
            (RingCode::DeliveryIndeterminate, RingPhase::Degraded, RingSeverity::Error)
        }
    }
}

/// Typed refusal of [`RingProducerPort::try_record`]; closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RingRefusal {
    /// The preallocated hand-off is full; the record is dropped and the producer counts the drop.
    Saturated,
    /// No ring is installed or it is gone; the record is dropped.
    Unavailable,
}

/// Producer side of the host's preallocated ring hand-off.
///
/// Contract for every implementation: `try_record` is non-blocking and non-allocating, does constant bounded
/// work, takes no lock and makes no syscall, so it may be called from a real-time thread. The trait itself
/// asks for nothing that blocks or allocates: the record is a fixed-size `Copy` value passed by reference
/// (no ownership transfer, no drop glue), the result is a `Copy` `Result<(), RingRefusal>`, and the trait
/// holds no `Mutex` or other lock. `&mut self` gives one producer handle per thread, matching the host's SPSC
/// `DiagRingWriter`; the trait is `Send`, not `Sync`. Real-time audio callbacks never call observe; this port
/// is for the host's preallocated hand-off, not for those callbacks. Proven for a test producer by
/// `tests/ring_port.rs` `producer_records_from_a_second_thread_and_saturation_is_typed` (zero allocations,
/// counting allocator); an implementation's own body is its owner's proof.
pub trait RingProducerPort: Send {
    fn try_record(&mut self, record: &RingRecord) -> Result<(), RingRefusal>;
}
