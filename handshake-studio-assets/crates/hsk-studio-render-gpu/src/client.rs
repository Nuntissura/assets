//! Typed GPU client port (lane SMB-GPU-CLIENT-PORT-1, RF-03 phase 0, MT-36248): render-gpu is a
//! client of the one Handshake GPU owner (WP-KERNEL-012 MT-203, not started). Every device use
//! asks the owner for admission ([`GpuClientPort::admit`]), carries the module, client and device
//! attribution of the request on its grant (GUI-DESKTOP-029), is authorised before every
//! submission ([`GpuClientPort::authorize`], a stale or revoked grant never submits:
//! GUI-DESKTOP-036), settles exactly once with measured residency numbers
//! ([`GpuClientPort::settle`], GPU-QUOTA-015) and rebinds through the owner after a device loss
//! ([`GpuClientPort::rebind`], generation + 1).
//!
//! Until the owner exists, [`TransientAdapter`] is the asset-local stand-in (its results are
//! evidence, never host acceptance: AC-HOST-005) and [`UnavailablePort`] the wgpu-free kill switch.
//! [`GrantLedger`] is the settle-once bookkeeping both use; a host port can use it as well. No
//! wgpu, ash or Vulkan type appears in any public item of this module (STU-ARC-002): the device a
//! grant hands to the backend travels as a crate-private lease.
#![deny(clippy::arithmetic_side_effects)]
use crate::device::DeviceLease;
use crate::error::Reason;
use hsk_studio_accord::CancellationToken;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

mod transient;
pub use transient::{TRANSIENT_DESCRIPTOR, TransientAdapter, TransientOptions};
#[cfg(feature = "asset-local-backends")]
pub(crate) use transient::Platform;
#[cfg(feature = "shared-device")]
mod external;
#[cfg(feature = "shared-device")]
pub(crate) use external::ExternalPort;

/// What the admitted work does on the device (closed; attribution of every allocation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuOperation {
    Compute,
    Upload,
    Readback,
    Display,
}
impl GpuOperation {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Compute => "compute",
            Self::Upload => "upload",
            Self::Readback => "readback",
            Self::Display => "display",
        }
    }
}

/// Opaque module identity (the host registry assigns it; 0 = unregistered asset-local use).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleRef(pub u32);
impl ModuleRef {
    pub const UNREGISTERED: Self = Self(0);
}
/// Opaque client identity within a module (0 = unregistered asset-local use).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientRef(pub u64);
impl ClientRef {
    pub const UNREGISTERED: Self = Self(0);
}
/// Opaque device identity assigned by the owner, or any device the owner chooses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DeviceRef {
    Any,
    Id(u64),
}

/// One admission request: the attribution every allocation of the grant carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionRequest {
    pub operation: GpuOperation,
    pub module: ModuleRef,
    pub client: ClientRef,
    pub device: DeviceRef,
    /// Bytes the client asks to reserve up front (0 = no up-front reservation; the backend charges
    /// every allocation to the grant's residency budget as it goes).
    pub requested_bytes: u64,
}
impl AdmissionRequest {
    pub const fn new(operation: GpuOperation, module: ModuleRef, client: ClientRef) -> Self {
        Self {
            operation,
            module,
            client,
            device: DeviceRef::Any,
            requested_bytes: 0,
        }
    }
    /// The request of the asset-local compatibility constructors (`GpuBackend::new`): compute,
    /// unregistered module and client, any device.
    pub const fn asset_local() -> Self {
        Self::new(
            GpuOperation::Compute,
            ModuleRef::UNREGISTERED,
            ClientRef::UNREGISTERED,
        )
    }
    pub const fn with_device(mut self, device: DeviceRef) -> Self {
        self.device = device;
        self
    }
    pub const fn with_requested_bytes(mut self, bytes: u64) -> Self {
        self.requested_bytes = bytes;
        self
    }
}

/// Opaque grant identity (unique within its issuing port).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GrantId(u64);
impl GrantId {
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Who issued a grant. `TransientAssetLocal` is the in-process stand-in: never host acceptance
/// (MT-36248 AC-HOST-005).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantIssuer {
    TransientAssetLocal,
    Host,
}
impl GrantIssuer {
    pub const fn key(self) -> &'static str {
        match self {
            Self::TransientAssetLocal => "transient_asset_local",
            Self::Host => "host",
        }
    }
}

/// Where a grant's residency budget comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetSource {
    /// Declared by Studio (`TransientOptions` / `DeviceOptions.residency_budget_bytes`), asset-local.
    StudioDeclared,
    /// Issued by the host owner's quota (GPU-QUOTA-010).
    HostIssued,
}
impl BudgetSource {
    pub const fn key(self) -> &'static str {
        match self {
            Self::StudioDeclared => "studio_declared",
            Self::HostIssued => "host_issued",
        }
    }
}

/// Plain-data snapshot of a grant (receipts, state queries); the grant itself is not `Clone`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrantInfo {
    pub id: GrantId,
    pub generation: u64,
    pub budget_bytes: u64,
    pub budget_source: BudgetSource,
    pub device: DeviceRef,
    /// Plain backend key of the device behind the grant (`vulkan`, `dx12`, `metal`, `gl`, `other`;
    /// empty when the grant carries no device).
    pub backend: &'static str,
    pub issuer: GrantIssuer,
    pub operation: GpuOperation,
    pub module: ModuleRef,
    pub client: ClientRef,
}

/// An admitted claim on one device generation. Not `Clone`: exactly one holder settles it once.
pub struct GpuGrant {
    info: GrantInfo,
    /// Identity of the issuing ledger: a grant of another port is a stale grant there.
    ledger: u64,
    /// Compact tombstone shared with the ledger's live record (R46 F-R46-5): how the owner closed
    /// the grant (`TOMB_*`) once the ledger dropped the record, so a closed grant is classified
    /// without a record kept for ever.
    tomb: Arc<AtomicU8>,
    /// The device the backend runs on (crate-private; taken once by the backend).
    lease: Option<Box<DeviceLease>>,
}
impl std::fmt::Debug for GpuGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuGrant")
            .field("info", &self.info)
            .field("lease", &self.lease.is_some())
            .finish()
    }
}
impl GpuGrant {
    pub fn info(&self) -> GrantInfo {
        self.info
    }
    pub fn id(&self) -> GrantId {
        self.info.id
    }
    pub fn generation(&self) -> u64 {
        self.info.generation
    }
    pub fn budget_bytes(&self) -> u64 {
        self.info.budget_bytes
    }
    pub fn device(&self) -> DeviceRef {
        self.info.device
    }
    pub fn backend(&self) -> &'static str {
        self.info.backend
    }
    pub fn issuer(&self) -> GrantIssuer {
        self.info.issuer
    }
    /// Whether a device still travels with the grant (taken by the backend on construction).
    pub fn carries_device(&self) -> bool {
        self.lease.is_some()
    }
    pub(crate) fn take_lease(&mut self) -> Option<Box<DeviceLease>> {
        self.lease.take()
    }
}

/// Observed, estimated and unknown byte numbers stay distinct (GPU-QUOTA-015).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteQuantity {
    Measured(u64),
    Estimated(u64),
    Unknown,
}

/// How the work under a grant ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettleOutcome {
    Completed,
    Canceled,
    Failed,
    Lost,
    /// The client was dropped without an explicit release (counted, never silent).
    Abandoned,
}
impl SettleOutcome {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Canceled => "canceled",
            Self::Failed => "failed",
            Self::Lost => "lost",
            Self::Abandoned => "abandoned",
        }
    }
}

/// The client's accounting at settlement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settlement {
    /// Bytes still charged to the grant's residency (measured charges).
    pub retained: ByteQuantity,
    /// Highest bytes charged over the grant's life (measured charges).
    pub peak: ByteQuantity,
    pub evictions: u64,
    /// Bytes the driver holds beyond the charges (render-gpu cannot observe them: `Unknown`).
    pub native_driver: ByteQuantity,
    /// Submissions not known to be finished; a settlement with any is refused as `InFlight`
    /// (GUI-DESKTOP-033/034).
    pub in_flight: u32,
    pub outcome: SettleOutcome,
}
impl Settlement {
    /// A settlement with no device work behind it (nothing retained, nothing in flight).
    pub const fn empty(outcome: SettleOutcome) -> Self {
        Self {
            retained: ByteQuantity::Measured(0),
            peak: ByteQuantity::Measured(0),
            evictions: 0,
            native_driver: ByteQuantity::Unknown,
            in_flight: 0,
            outcome,
        }
    }
}

/// Proof of one settlement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettleReceipt {
    pub grant: GrantInfo,
    pub settlement: Settlement,
}

/// Why an admission or rebind is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenialCode {
    BudgetExceeded,
    QuotaUnavailable,
    Revoked,
}
impl DenialCode {
    pub const fn code(self) -> &'static str {
        match self {
            Self::BudgetExceeded => "grant_budget_exceeded",
            Self::QuotaUnavailable => "grant_quota_unavailable",
            Self::Revoked => "grant_revoked",
        }
    }
}

/// Typed refusal of [`GpuClientPort::admit`], [`GpuClientPort::authorize`] and
/// [`GpuClientPort::rebind`]. No silent CPU fallback exists behind any of them (AC-HOST-004).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientRefusal {
    Unavailable(Reason),
    /// The device of `generation` is lost: rebind through the owner.
    Lost { generation: u64 },
    Denied(DenialCode),
    Canceled,
}
impl ClientRefusal {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unavailable(reason) => reason.code(),
            Self::Lost { .. } => "device_lost",
            Self::Denied(code) => code.code(),
            Self::Canceled => "canceled",
        }
    }
}

/// Typed refusal of [`GpuClientPort::settle`]; a refused settlement changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettleError {
    /// The grant was already settled.
    DoubleRelease,
    /// The owner already closed the grant (it was superseded by a rebind).
    LateRelease,
    /// The grant is unknown to this port (another port's, or not issued here).
    StaleGrant,
    /// Work under the grant may still run; settle again once it finished.
    InFlight,
}
impl SettleError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::DoubleRelease => "double_release",
            Self::LateRelease => "late_release",
            Self::StaleGrant => "stale_grant",
            Self::InFlight => "settle_in_flight",
        }
    }
}
impl std::fmt::Display for SettleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for SettleError {}

/// Every terminal port outcome as a closed code (rule 5: codes and counters, never text); emitted
/// through `report::report_port` and counted in [`PortCounters`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortEvent {
    Admitted,
    Refused(ClientRefusal),
    Settled(SettleOutcome),
    SettleRefused(SettleError),
    Rebound { generation: u64 },
}
impl PortEvent {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Admitted => "grant_admitted",
            Self::Refused(refusal) => refusal.code(),
            Self::Settled(SettleOutcome::Abandoned) => "grant_abandoned",
            Self::Settled(_) => "grant_settled",
            Self::SettleRefused(error) => error.code(),
            Self::Rebound { .. } => "grant_rebound",
        }
    }
}

/// Closed counters of one port (no text, no ids).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PortCounters {
    pub admitted: u64,
    pub canceled: u64,
    pub unavailable: u64,
    pub lost: u64,
    pub denied: u64,
    pub settled: u64,
    pub abandoned: u64,
    pub double_release: u64,
    pub late_release: u64,
    pub stale_grant: u64,
    pub in_flight: u64,
    pub rebound: u64,
    /// Grants admitted and neither settled nor superseded.
    pub outstanding: u64,
}
impl PortCounters {
    fn count(&mut self, event: PortEvent) {
        let slot = match event {
            PortEvent::Admitted => &mut self.admitted,
            PortEvent::Refused(ClientRefusal::Canceled) => &mut self.canceled,
            PortEvent::Refused(ClientRefusal::Unavailable(_)) => &mut self.unavailable,
            PortEvent::Refused(ClientRefusal::Lost { .. }) => &mut self.lost,
            PortEvent::Refused(ClientRefusal::Denied(_)) => &mut self.denied,
            PortEvent::Settled(SettleOutcome::Abandoned) => &mut self.abandoned,
            PortEvent::Settled(_) => &mut self.settled,
            PortEvent::SettleRefused(SettleError::DoubleRelease) => &mut self.double_release,
            PortEvent::SettleRefused(SettleError::LateRelease) => &mut self.late_release,
            PortEvent::SettleRefused(SettleError::StaleGrant) => &mut self.stale_grant,
            PortEvent::SettleRefused(SettleError::InFlight) => &mut self.in_flight,
            PortEvent::Rebound { .. } => &mut self.rebound,
        };
        *slot = slot.saturating_add(1);
    }
}

/// How a fault-injection hook closes a held grant (feature `test-faults`, V4-04; R46 F-R46-3).
#[cfg(feature = "test-faults")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantClosure {
    /// The owner revoked the grant: later submissions are `Denied(Revoked)`.
    Revoked,
    /// The owner rebound the grant elsewhere: later submissions are `Lost { generation }`.
    Superseded,
}

/// The client contract toward the one GPU owner. Implemented by the host owner (MT-203) and, until
/// it exists, by [`TransientAdapter`] (asset-local) and [`UnavailablePort`].
pub trait GpuClientPort: Send + Sync {
    /// Admit `request`: a grant carrying its attribution, or a typed refusal. A canceled token is
    /// `Canceled` before any device work.
    fn admit(
        &self,
        request: &AdmissionRequest,
        cancel: &CancellationToken,
    ) -> Result<GpuGrant, ClientRefusal>;
    /// Whether `grant` may submit now (called before every submission). A grant superseded by a
    /// rebind is `Lost { generation }`, a settled, revoked or unknown one `Denied(Revoked)`.
    fn authorize(&self, grant: &GpuGrant) -> Result<(), ClientRefusal>;
    /// Settle `grant` exactly once.
    fn settle(&self, grant: &GpuGrant, settlement: Settlement)
    -> Result<SettleReceipt, SettleError>;
    /// After a device loss: a grant on the successor device (generation + 1) with the same
    /// attribution; the old grant is closed by the owner (a later settle of it is `LateRelease`).
    /// A failed rebind keeps the old grant open.
    fn rebind(
        &self,
        grant: &GpuGrant,
        cancel: &CancellationToken,
    ) -> Result<GpuGrant, ClientRefusal>;
    fn counters(&self) -> PortCounters;
    /// Fault injection (feature `test-faults`, V4-04; R46 F-R46-3): close `grant` as the owner
    /// would. A port without the hook closes nothing (`false`).
    #[cfg(feature = "test-faults")]
    fn inject_closure(&self, grant: &GpuGrant, closure: GrantClosure) -> bool {
        let _ = (grant, closure);
        false
    }
}

/// The wgpu-free kill switch: no device, ever (`Unavailable(no_adapter)`); replaces the old
/// `BackendChoice::None` in tests.
#[derive(Debug, Default)]
pub struct UnavailablePort {
    counters: Mutex<PortCounters>,
}
impl UnavailablePort {
    pub fn new() -> Self {
        Self::default()
    }
    fn refuse<T>(&self, event: PortEvent, error: T) -> T {
        self.counters
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .count(event);
        error
    }
}
impl GpuClientPort for UnavailablePort {
    fn admit(&self, _: &AdmissionRequest, _: &CancellationToken) -> Result<GpuGrant, ClientRefusal> {
        let refusal = ClientRefusal::Unavailable(Reason::NoAdapter);
        Err(self.refuse(PortEvent::Refused(refusal), refusal))
    }
    fn authorize(&self, _: &GpuGrant) -> Result<(), ClientRefusal> {
        let refusal = ClientRefusal::Denied(DenialCode::Revoked);
        Err(self.refuse(PortEvent::Refused(refusal), refusal))
    }
    fn settle(&self, _: &GpuGrant, _: Settlement) -> Result<SettleReceipt, SettleError> {
        let error = SettleError::StaleGrant;
        Err(self.refuse(PortEvent::SettleRefused(error), error))
    }
    fn rebind(&self, _: &GpuGrant, _: &CancellationToken) -> Result<GpuGrant, ClientRefusal> {
        let refusal = ClientRefusal::Unavailable(Reason::NoAdapter);
        Err(self.refuse(PortEvent::Refused(refusal), refusal))
    }
    fn counters(&self) -> PortCounters {
        *self.counters.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantState {
    Open,
    Settled,
    /// Closed by the owner on a rebind; the successor has a higher generation.
    Superseded,
    Revoked,
}
/// Tombstone values of a grant (`GpuGrant::tomb`).
const TOMB_LIVE: u8 = 0;
const TOMB_SETTLED: u8 = 1;
const TOMB_SUPERSEDED: u8 = 2;
/// The ledger's record of a live grant (open, or revoked and not yet settled).
#[derive(Debug)]
struct Record {
    generation: u64,
    /// `Open` or `Revoked` (settled and superseded grants have no record).
    state: GrantState,
    tomb: Arc<AtomicU8>,
}
/// Bounded bookkeeping (R46 F-R46-5): only live grants keep a record; a settled or superseded
/// grant's record is dropped and its state survives in the grant's own tombstone. Ids are
/// monotone, so an id this ledger issued without a record is closed. `open` counts the open
/// grants (no scan).
#[derive(Default)]
struct LedgerState {
    next_id: u64,
    live: BTreeMap<u64, Record>,
    open: u64,
    counters: PortCounters,
}
impl LedgerState {
    fn event(&mut self, event: PortEvent) {
        self.counters.count(event);
    }
    /// Drop the record of `id` and close its grant with tombstone `tomb`; an open grant leaves the
    /// open count.
    fn close(&mut self, id: u64, tomb: u8) {
        if let Some(record) = self.live.remove(&id) {
            record.tomb.store(tomb, Ordering::Release);
            if record.state == GrantState::Open {
                self.open = self.open.saturating_sub(1);
            }
        }
    }
}

/// Unique ledger identities within the process (a grant names the ledger that issued it).
static NEXT_LEDGER: AtomicU64 = AtomicU64::new(1);

/// Settle-once bookkeeping of one port: grant ids, generations, open/settled/superseded/revoked
/// state and the closed counters. A port wraps it; [`GrantLedger::issue_host`] lets a host port
/// (or a test port) issue `Host` grants without a device lease.
pub struct GrantLedger {
    key: u64,
    state: Mutex<LedgerState>,
}
impl Default for GrantLedger {
    fn default() -> Self {
        Self::new()
    }
}
impl std::fmt::Debug for GrantLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrantLedger")
            .field("key", &self.key)
            .field("counters", &self.counters())
            .finish()
    }
}

/// Everything a new grant needs besides its id (crate-internal issue path).
pub(crate) struct GrantParts {
    pub(crate) generation: u64,
    pub(crate) budget_bytes: u64,
    pub(crate) budget_source: BudgetSource,
    pub(crate) device: DeviceRef,
    pub(crate) backend: &'static str,
    pub(crate) issuer: GrantIssuer,
    pub(crate) lease: Option<Box<DeviceLease>>,
}

impl GrantLedger {
    pub fn new() -> Self {
        Self {
            key: NEXT_LEDGER.fetch_add(1, Ordering::Relaxed),
            state: Mutex::new(LedgerState::default()),
        }
    }
    fn lock(&self) -> MutexGuard<'_, LedgerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
    /// Count a refusal decided by the port before the ledger was asked (cancel, no device).
    pub fn refuse(&self, refusal: ClientRefusal) -> ClientRefusal {
        self.lock().event(PortEvent::Refused(refusal));
        refusal
    }
    /// Admission budget check: `requested_bytes` above the budget is `Denied(BudgetExceeded)`.
    pub fn check_budget(&self, request: &AdmissionRequest, budget_bytes: u64) -> Result<(), ClientRefusal> {
        if request.requested_bytes > budget_bytes {
            return Err(self.refuse(ClientRefusal::Denied(DenialCode::BudgetExceeded)));
        }
        Ok(())
    }
    /// Issue a `Host` grant (generation 1, no device lease) for `request`; refused as
    /// `Denied(BudgetExceeded)` when the request does not fit `budget_bytes`.
    pub fn issue_host(
        &self,
        request: &AdmissionRequest,
        budget_bytes: u64,
        device: u64,
        backend: &'static str,
    ) -> Result<GpuGrant, ClientRefusal> {
        self.check_budget(request, budget_bytes)?;
        Ok(self.issue(
            request,
            GrantParts {
                generation: 1,
                budget_bytes,
                budget_source: BudgetSource::HostIssued,
                device: DeviceRef::Id(device),
                backend,
                issuer: GrantIssuer::Host,
                lease: None,
            },
        ))
    }
    pub(crate) fn issue(&self, request: &AdmissionRequest, parts: GrantParts) -> GpuGrant {
        let mut state = self.lock();
        state.next_id = state.next_id.saturating_add(1);
        let id = state.next_id;
        let tomb = Arc::new(AtomicU8::new(TOMB_LIVE));
        state.live.insert(
            id,
            Record {
                generation: parts.generation,
                state: GrantState::Open,
                tomb: Arc::clone(&tomb),
            },
        );
        state.open = state.open.saturating_add(1);
        state.event(PortEvent::Admitted);
        GpuGrant {
            info: GrantInfo {
                id: GrantId(id),
                generation: parts.generation,
                budget_bytes: parts.budget_bytes,
                budget_source: parts.budget_source,
                device: parts.device,
                backend: parts.backend,
                issuer: parts.issuer,
                operation: request.operation,
                module: request.module,
                client: request.client,
            },
            ledger: self.key,
            tomb,
            lease: parts.lease,
        }
    }
    /// The state of `grant` here, or `None` for a grant this ledger did not issue (or whose
    /// generation differs from its record: of another lineage). A live grant answers from its
    /// record, a closed one (an issued id without a record) from its tombstone.
    fn state_of(state: &LedgerState, key: u64, grant: &GpuGrant) -> Option<GrantState> {
        if grant.ledger != key || grant.info.id.0 == 0 || grant.info.id.0 > state.next_id {
            return None;
        }
        match state.live.get(&grant.info.id.0) {
            Some(r) if Arc::ptr_eq(&r.tomb, &grant.tomb) && r.generation == grant.info.generation => {
                Some(r.state)
            }
            Some(_) => None,
            None => match grant.tomb.load(Ordering::Acquire) {
                TOMB_SETTLED => Some(GrantState::Settled),
                TOMB_SUPERSEDED => Some(GrantState::Superseded),
                _ => None,
            },
        }
    }
    pub fn authorize(&self, grant: &GpuGrant) -> Result<(), ClientRefusal> {
        let mut state = self.lock();
        let refusal = match Self::state_of(&state, self.key, grant) {
            Some(GrantState::Open) => return Ok(()),
            Some(GrantState::Superseded) => ClientRefusal::Lost {
                generation: grant.info.generation,
            },
            Some(GrantState::Settled | GrantState::Revoked) | None => {
                ClientRefusal::Denied(DenialCode::Revoked)
            }
        };
        state.event(PortEvent::Refused(refusal));
        Err(refusal)
    }
    pub fn settle(
        &self,
        grant: &GpuGrant,
        settlement: Settlement,
    ) -> Result<SettleReceipt, SettleError> {
        let mut state = self.lock();
        let verdict = match Self::state_of(&state, self.key, grant) {
            None => Err(SettleError::StaleGrant),
            Some(GrantState::Settled) => Err(SettleError::DoubleRelease),
            Some(GrantState::Superseded) => Err(SettleError::LateRelease),
            // A revoked grant still settles once: the client reports what it held.
            Some(GrantState::Open | GrantState::Revoked) if settlement.in_flight > 0 => {
                Err(SettleError::InFlight)
            }
            Some(GrantState::Open | GrantState::Revoked) => Ok(()),
        };
        match verdict {
            Ok(()) => {
                state.close(grant.info.id.0, TOMB_SETTLED);
                state.event(PortEvent::Settled(settlement.outcome));
                Ok(SettleReceipt {
                    grant: grant.info,
                    settlement,
                })
            }
            Err(error) => {
                state.event(PortEvent::SettleRefused(error));
                Err(error)
            }
        }
    }
    /// Whether `grant` may be rebound: its generation when it is the open grant of its lineage.
    pub fn check_rebind(&self, grant: &GpuGrant) -> Result<u64, ClientRefusal> {
        let mut state = self.lock();
        let refusal = match Self::state_of(&state, self.key, grant) {
            Some(GrantState::Open) => return Ok(grant.info.generation),
            Some(GrantState::Superseded) => ClientRefusal::Lost {
                generation: grant.info.generation,
            },
            Some(GrantState::Settled | GrantState::Revoked) | None => {
                ClientRefusal::Denied(DenialCode::Revoked)
            }
        };
        state.event(PortEvent::Refused(refusal));
        Err(refusal)
    }
    /// Host-side rebind without a device lease: close `old` as superseded and issue its `Host`
    /// successor (generation + 1, same attribution and device, `budget_bytes`).
    pub fn rebind_host(
        &self,
        old: &GpuGrant,
        budget_bytes: u64,
        backend: &'static str,
    ) -> Result<GpuGrant, ClientRefusal> {
        self.check_rebind(old)?;
        self.rebind_with(
            old,
            GrantParts {
                generation: 0,
                budget_bytes,
                budget_source: BudgetSource::HostIssued,
                device: old.info.device,
                backend,
                issuer: GrantIssuer::Host,
                lease: None,
            },
        )
    }
    /// Close `old` as superseded and issue its successor (generation + 1, same attribution) in one
    /// step; refused when `old` stopped being open meanwhile.
    pub(crate) fn rebind_with(
        &self,
        old: &GpuGrant,
        mut parts: GrantParts,
    ) -> Result<GpuGrant, ClientRefusal> {
        let request = AdmissionRequest {
            operation: old.info.operation,
            module: old.info.module,
            client: old.info.client,
            device: old.info.device,
            requested_bytes: 0,
        };
        {
            let mut state = self.lock();
            let open = Self::state_of(&state, self.key, old) == Some(GrantState::Open);
            if !open {
                let refusal = ClientRefusal::Denied(DenialCode::Revoked);
                state.event(PortEvent::Refused(refusal));
                return Err(refusal);
            }
            state.close(old.info.id.0, TOMB_SUPERSEDED);
            parts.generation = old.info.generation.saturating_add(1);
            state.event(PortEvent::Rebound {
                generation: parts.generation,
            });
        }
        Ok(self.issue(&request, parts))
    }
    /// Host-side revocation: the grant is refused for every later submission (`Denied(Revoked)`)
    /// and still settles once. `false` for a grant that is not open here.
    pub fn revoke(&self, grant: &GpuGrant) -> bool {
        let mut state = self.lock();
        let open = Self::state_of(&state, self.key, grant) == Some(GrantState::Open);
        if open && let Some(record) = state.live.get_mut(&grant.info.id.0) {
            record.state = GrantState::Revoked;
            state.open = state.open.saturating_sub(1);
        }
        open
    }
    /// Fault injection (feature `test-faults`, V4-04; R46 F-R46-3): close an open `grant` as the
    /// owner would, revoked, or superseded without a successor (a rebind elsewhere). `false` when
    /// it is not open here.
    #[cfg(feature = "test-faults")]
    pub fn inject_closure(&self, grant: &GpuGrant, closure: GrantClosure) -> bool {
        match closure {
            GrantClosure::Revoked => self.revoke(grant),
            GrantClosure::Superseded => {
                let mut state = self.lock();
                let open = Self::state_of(&state, self.key, grant) == Some(GrantState::Open);
                if open {
                    state.close(grant.info.id.0, TOMB_SUPERSEDED);
                }
                open
            }
        }
    }
    /// Records the ledger holds: live grants only (open, or revoked and not yet settled); settled
    /// and superseded grants keep no record (R46 F-R46-5).
    pub fn live_records(&self) -> usize {
        self.lock().live.len()
    }
    pub fn counters(&self) -> PortCounters {
        let mut state = self.lock();
        state.counters.outstanding = state.open;
        state.counters
    }
}
