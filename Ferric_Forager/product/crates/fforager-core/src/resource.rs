//! Atomic coupled-resource and byte-credit accounting models.
//!
//! Raw ledgers and transition identities are deliberately crate-private. External
//! consumers must hold the owned boundaries so drop, cancellation, and release
//! mutate the authoritative model exactly once.
//!
//! Regression: `WP009-REG-PUBLIC-OWNERSHIP-ESCAPE-001`.
//!
//! ```compile_fail
//! use fforager_core::resource::{ByteCreditLedger, ResourceLedger};
//! ```
//!
//! Durability positions cannot be advanced without a core-issued effect token.
//!
//! ```compile_fail
//! use fforager_core::resource::{
//!     ByteCreditPosition, DurabilityAcknowledgement, OwnedByteCreditBroker,
//! };
//!
//! fn forge(broker: &OwnedByteCreditBroker) {
//!     let token = DurabilityAcknowledgement {
//!         ledger: std::rc::Weak::new(),
//!         next: ByteCreditPosition::default(),
//!     };
//!     broker.advance(token).unwrap();
//! }
//! ```

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    rc::{Rc, Weak},
};

pub use fforager_contracts::{
    ByteCreditComponent, ByteCreditContractV1, ByteCreditPosition, ByteCreditSaturationPolicy,
    ByteCreditStage, CoupledByteReservation, CoupledByteReservationError, ResourceContractV1,
    ResourceVector,
};

/// Stable identity of the job/owner charged for resources and byte credits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OwnerId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GrantId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WaiterId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Grant {
    id: GrantId,
    owner: OwnerId,
    resources: ResourceVector,
    /// The queue identity that produced this grant, or `None` for direct admission.
    source_waiter: Option<WaiterId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    Granted(Grant),
    Queued(WaiterId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LedgerError {
    InvalidConfiguration,
    RequestExceedsCapacity,
    QueueItemLimit,
    QueueByteLimit,
    ArithmeticOverflow,
    IdExhausted,
    UnknownGrant(GrantId),
    GrantAlreadyReleased(GrantId),
    GrantOwnerMismatch { expected: OwnerId, actual: OwnerId },
    UnknownWaiter(WaiterId),
    WaiterAlreadyResolved(WaiterId),
    BrokerDropped,
    InvariantViolation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Waiter {
    id: WaiterId,
    owner: OwnerId,
    resources: ResourceVector,
    variable_bytes: u64,
}

/// A deterministic FIFO broker with all-or-nothing coupled admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResourceLedger {
    capacity: ResourceVector,
    in_use: ResourceVector,
    active: BTreeMap<GrantId, Grant>,
    waiters: VecDeque<Waiter>,
    max_active_grants: usize,
    max_active_per_owner: usize,
    max_waiters: usize,
    max_waiter_bytes: u64,
    waiter_bytes: u64,
    next_grant: u64,
    next_waiter: u64,
}

impl ResourceLedger {
    #[cfg(test)]
    #[must_use]
    pub(crate) fn new(
        capacity: ResourceVector,
        max_active_grants: usize,
        max_waiters: usize,
        max_waiter_bytes: u64,
    ) -> Self {
        Self {
            capacity,
            in_use: ResourceVector::default(),
            active: BTreeMap::new(),
            waiters: VecDeque::new(),
            max_active_grants,
            max_active_per_owner: max_active_grants.div_ceil(2),
            max_waiters,
            max_waiter_bytes,
            waiter_bytes: 0,
            next_grant: 1,
            next_waiter: 1,
        }
    }

    /// Build a ledger with the exact declared per-owner active ceiling.
    ///
    /// # Errors
    ///
    /// Returns `InvalidConfiguration` when the active limits are zero or the
    /// owner ceiling exceeds the total active-grant bound.
    pub(crate) fn new_with_owner_ceiling(
        capacity: ResourceVector,
        max_active_grants: usize,
        max_active_per_owner: usize,
        max_waiters: usize,
        max_waiter_bytes: u64,
    ) -> Result<Self, LedgerError> {
        if max_active_grants == 0
            || max_active_per_owner == 0
            || max_active_per_owner > max_active_grants
        {
            return Err(LedgerError::InvalidConfiguration);
        }
        Ok(Self {
            capacity,
            in_use: ResourceVector::default(),
            active: BTreeMap::new(),
            waiters: VecDeque::new(),
            max_active_grants,
            max_active_per_owner,
            max_waiters,
            max_waiter_bytes,
            waiter_bytes: 0,
            next_grant: 1,
            next_waiter: 1,
        })
    }

    /// Build the pure ledger from a validated versioned v1 contract.
    ///
    /// # Errors
    ///
    /// Returns `InvalidConfiguration` for schema/bound or host-size mismatch.
    pub(crate) fn from_contract(contract: &ResourceContractV1) -> Result<Self, LedgerError> {
        contract
            .validate()
            .map_err(|_| LedgerError::InvalidConfiguration)?;
        let max_active_grants = usize::try_from(contract.max_active_grants)
            .map_err(|_| LedgerError::InvalidConfiguration)?;
        let max_active_per_owner = usize::try_from(contract.max_active_per_owner)
            .map_err(|_| LedgerError::InvalidConfiguration)?;
        let max_waiters = usize::try_from(contract.max_waiter_items)
            .map_err(|_| LedgerError::InvalidConfiguration)?;
        Self::new_with_owner_ceiling(
            contract.capacity,
            max_active_grants,
            max_active_per_owner,
            max_waiters,
            contract.max_waiter_declared_variable_bytes,
        )
    }

    #[must_use]
    fn in_use(&self) -> ResourceVector {
        self.in_use
    }

    #[must_use]
    fn active_grant_count(&self) -> usize {
        self.active.len()
    }

    #[must_use]
    fn waiter_occupancy(&self) -> (usize, u64) {
        (self.waiters.len(), self.waiter_bytes)
    }

    /// Atomically grant or boundedly queue the complete vector.
    ///
    /// # Errors
    ///
    /// Returns a typed capacity, bound, arithmetic, or identity error.
    fn request(
        &mut self,
        owner: OwnerId,
        resources: ResourceVector,
    ) -> Result<Admission, LedgerError> {
        if !resources.fits_within(self.capacity) {
            return Err(LedgerError::RequestExceedsCapacity);
        }
        if self.waiters.is_empty()
            && self.active.len() < self.max_active_grants
            && self.owner_active_count(owner) < self.max_active_per_owner
            && self.can_grant(resources)
        {
            return self.issue(owner, resources, None).map(Admission::Granted);
        }
        if self.waiters.len() >= self.max_waiters {
            return Err(LedgerError::QueueItemLimit);
        }
        let variable_bytes = resources
            .declared_variable_bytes()
            .ok_or(LedgerError::ArithmeticOverflow)?;
        let new_waiter_bytes = self
            .waiter_bytes
            .checked_add(variable_bytes)
            .ok_or(LedgerError::ArithmeticOverflow)?;
        if new_waiter_bytes > self.max_waiter_bytes {
            return Err(LedgerError::QueueByteLimit);
        }
        let id = WaiterId(self.next_waiter);
        self.next_waiter = self
            .next_waiter
            .checked_add(1)
            .ok_or(LedgerError::IdExhausted)?;
        self.waiters.push_back(Waiter {
            id,
            owner,
            resources,
            variable_bytes,
        });
        self.waiter_bytes = new_waiter_bytes;
        Ok(Admission::Queued(id))
    }

    /// Cancel a queued request and dispatch newly unblocked FIFO waiters.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown waiter or broken accounting invariant.
    fn cancel_waiter(&mut self, id: WaiterId) -> Result<Vec<Grant>, LedgerError> {
        let mut candidate = self.clone();
        let issued = candidate.cancel_waiter_checked(id)?;
        *self = candidate;
        Ok(issued)
    }

    fn cancel_waiter_checked(&mut self, id: WaiterId) -> Result<Vec<Grant>, LedgerError> {
        let Some(position) = self.waiters.iter().position(|waiter| waiter.id == id) else {
            return Err(LedgerError::UnknownWaiter(id));
        };
        let Some(waiter) = self.waiters.remove(position) else {
            return Err(LedgerError::InvariantViolation);
        };
        self.waiter_bytes = self
            .waiter_bytes
            .checked_sub(waiter.variable_bytes)
            .ok_or(LedgerError::InvariantViolation)?;
        self.drain_waiters()
    }

    /// Release exactly one owned grant and admit FIFO waiters that now fit.
    ///
    /// # Errors
    ///
    /// Returns a typed unknown, duplicate, ownership, or invariant error.
    fn release(&mut self, id: GrantId, owner: OwnerId) -> Result<Vec<Grant>, LedgerError> {
        let mut candidate = self.clone();
        let issued = candidate.release_checked(id, owner)?;
        *self = candidate;
        Ok(issued)
    }

    fn release_checked(&mut self, id: GrantId, owner: OwnerId) -> Result<Vec<Grant>, LedgerError> {
        let Some(grant) = self.active.get(&id).copied() else {
            return if id.0 > 0 && id.0 < self.next_grant {
                Err(LedgerError::GrantAlreadyReleased(id))
            } else {
                Err(LedgerError::UnknownGrant(id))
            };
        };
        if grant.owner != owner {
            return Err(LedgerError::GrantOwnerMismatch {
                expected: grant.owner,
                actual: owner,
            });
        }
        self.in_use = self
            .in_use
            .checked_sub(grant.resources)
            .ok_or(LedgerError::InvariantViolation)?;
        self.active.remove(&id);
        self.drain_waiters()
    }

    /// Recompute all accounting and bounds from the exact active identities.
    ///
    /// # Errors
    ///
    /// Returns `InvariantViolation` for any mismatch or exceeded bound.
    fn verify(&self) -> Result<(), LedgerError> {
        let mut sum = ResourceVector::default();
        for grant in self.active.values() {
            sum = sum
                .checked_add(grant.resources)
                .ok_or(LedgerError::InvariantViolation)?;
        }
        let bytes = self.waiters.iter().try_fold(0_u64, |total, waiter| {
            total.checked_add(waiter.variable_bytes)
        });
        if sum != self.in_use
            || !self.in_use.fits_within(self.capacity)
            || bytes != Some(self.waiter_bytes)
            || self.active.len() > self.max_active_grants
            || self
                .active
                .values()
                .map(|grant| grant.owner)
                .collect::<BTreeSet<_>>()
                .iter()
                .any(|owner| self.owner_active_count(*owner) > self.max_active_per_owner)
            || self.waiters.len() > self.max_waiters
            || self.waiter_bytes > self.max_waiter_bytes
        {
            return Err(LedgerError::InvariantViolation);
        }
        Ok(())
    }

    fn can_grant(&self, resources: ResourceVector) -> bool {
        let Some(combined) = self.in_use.checked_add(resources) else {
            return false;
        };
        combined.fits_within(self.capacity)
    }

    fn issue(
        &mut self,
        owner: OwnerId,
        resources: ResourceVector,
        source_waiter: Option<WaiterId>,
    ) -> Result<Grant, LedgerError> {
        let combined = self
            .in_use
            .checked_add(resources)
            .ok_or(LedgerError::ArithmeticOverflow)?;
        if !combined.fits_within(self.capacity) {
            return Err(LedgerError::RequestExceedsCapacity);
        }
        let id = GrantId(self.next_grant);
        self.next_grant = self
            .next_grant
            .checked_add(1)
            .ok_or(LedgerError::IdExhausted)?;
        let grant = Grant {
            id,
            owner,
            resources,
            source_waiter,
        };
        self.active.insert(id, grant);
        self.in_use = combined;
        Ok(grant)
    }

    fn drain_waiters(&mut self) -> Result<Vec<Grant>, LedgerError> {
        let mut issued = Vec::new();
        while self.active.len() < self.max_active_grants {
            let Some(waiter) = self.waiters.front().copied() else {
                break;
            };
            if self.owner_active_count(waiter.owner) >= self.max_active_per_owner
                || !self.can_grant(waiter.resources)
            {
                break;
            }
            let Some(waiter) = self.waiters.pop_front() else {
                return Err(LedgerError::InvariantViolation);
            };
            self.waiter_bytes = self
                .waiter_bytes
                .checked_sub(waiter.variable_bytes)
                .ok_or(LedgerError::InvariantViolation)?;
            issued.push(self.issue(waiter.owner, waiter.resources, Some(waiter.id))?);
        }
        Ok(issued)
    }

    fn owner_active_count(&self, owner: OwnerId) -> usize {
        self.active
            .values()
            .filter(|grant| grant.owner == owner)
            .count()
    }
}

#[derive(Clone, Debug)]
struct OwnedResourceState {
    ledger: ResourceLedger,
    ready: BTreeMap<WaiterId, Grant>,
}

impl OwnedResourceState {
    fn record_issued(&mut self, issued: Vec<Grant>) -> Result<Vec<WaiterId>, LedgerError> {
        let mut ready_ids = Vec::with_capacity(issued.len());
        let mut staged = Vec::with_capacity(issued.len());
        let mut seen = BTreeSet::new();
        for grant in issued {
            let Some(waiter_id) = grant.source_waiter else {
                return Err(LedgerError::InvariantViolation);
            };
            if self.ready.contains_key(&waiter_id) || !seen.insert(waiter_id) {
                return Err(LedgerError::InvariantViolation);
            }
            ready_ids.push(waiter_id);
            staged.push((waiter_id, grant));
        }
        for (waiter_id, grant) in staged {
            self.ready.insert(waiter_id, grant);
        }
        Ok(ready_ids)
    }

    fn release_grant(
        &mut self,
        grant_id: GrantId,
        owner: OwnerId,
    ) -> Result<Vec<WaiterId>, LedgerError> {
        let mut candidate = self.clone();
        let issued = candidate.ledger.release(grant_id, owner)?;
        let ready = candidate.record_issued(issued)?;
        *self = candidate;
        Ok(ready)
    }

    fn cancel_waiter(&mut self, waiter_id: WaiterId) -> Result<Vec<WaiterId>, LedgerError> {
        let mut candidate = self.clone();
        let issued = if let Some(grant) = candidate.ready.remove(&waiter_id) {
            candidate.ledger.release(grant.id, grant.owner)?
        } else {
            candidate.ledger.cancel_waiter(waiter_id)?
        };
        let ready = candidate.record_issued(issued)?;
        *self = candidate;
        Ok(ready)
    }
}

/// Single-threaded owner of the pure resource ledger.
///
/// Cloning the broker clones only the handle. Grants and waiters remain unique,
/// non-cloneable identities whose `Drop` paths perform real ledger mutation.
#[derive(Clone, Debug)]
pub struct OwnedResourceBroker {
    state: Rc<RefCell<OwnedResourceState>>,
}

impl OwnedResourceBroker {
    #[must_use]
    fn from_ledger(ledger: ResourceLedger) -> Self {
        Self {
            state: Rc::new(RefCell::new(OwnedResourceState {
                ledger,
                ready: BTreeMap::new(),
            })),
        }
    }

    /// Build an owned broker from the validated v1 contract.
    ///
    /// # Errors
    ///
    /// Returns `InvalidConfiguration` for a rejected contract or host-size mismatch.
    pub fn from_contract(contract: &ResourceContractV1) -> Result<Self, LedgerError> {
        ResourceLedger::from_contract(contract).map(Self::from_ledger)
    }

    /// Atomically admit or boundedly queue the complete resource vector.
    ///
    /// # Errors
    ///
    /// Returns the same typed errors as the pure ledger.
    pub fn request(
        &self,
        owner: OwnerId,
        resources: ResourceVector,
    ) -> Result<OwnedAdmission, LedgerError> {
        let admission = self.state.borrow_mut().ledger.request(owner, resources)?;
        Ok(match admission {
            Admission::Granted(grant) => OwnedAdmission::Granted(OwnedResourceLease {
                state: Rc::downgrade(&self.state),
                grant,
                active: true,
            }),
            Admission::Queued(waiter_id) => OwnedAdmission::Queued(OwnedResourceWaiter {
                state: Rc::downgrade(&self.state),
                waiter_id,
                active: true,
            }),
        })
    }

    #[must_use]
    pub fn in_use(&self) -> ResourceVector {
        self.state.borrow().ledger.in_use()
    }

    #[must_use]
    pub fn active_grant_count(&self) -> usize {
        self.state.borrow().ledger.active_grant_count()
    }

    #[must_use]
    pub fn waiter_occupancy(&self) -> (usize, u64) {
        self.state.borrow().ledger.waiter_occupancy()
    }

    #[must_use]
    pub fn ready_waiter_ids(&self) -> Vec<WaiterId> {
        self.state.borrow().ready.keys().copied().collect()
    }

    /// Verify both pure-ledger invariants and ready-grant identity mapping.
    ///
    /// # Errors
    ///
    /// Returns `InvariantViolation` for a missing waiter origin or ledger mismatch.
    pub fn verify(&self) -> Result<(), LedgerError> {
        let state = self.state.borrow();
        state.ledger.verify()?;
        if state.ready.iter().any(|(waiter_id, grant)| {
            grant.source_waiter != Some(*waiter_id)
                || state.ledger.active.get(&grant.id) != Some(grant)
        }) {
            return Err(LedgerError::InvariantViolation);
        }
        Ok(())
    }
}

#[derive(Debug)]
#[must_use = "dropping an admission releases or cancels its owned identity"]
pub enum OwnedAdmission {
    Granted(OwnedResourceLease),
    Queued(OwnedResourceWaiter),
}

/// A non-cloneable owned grant. Drop releases its full vector exactly once.
#[derive(Debug)]
#[must_use = "dropping the lease releases its complete resource vector"]
pub struct OwnedResourceLease {
    state: Weak<RefCell<OwnedResourceState>>,
    grant: Grant,
    active: bool,
}

impl OwnedResourceLease {
    #[must_use]
    pub fn owner(&self) -> OwnerId {
        self.grant.owner
    }

    #[must_use]
    pub fn resources(&self) -> ResourceVector {
        self.grant.resources
    }

    /// Explicitly release this lease and report FIFO waiters made ready.
    ///
    /// # Errors
    ///
    /// Returns a ledger invariant error or `BrokerDropped` when no ledger remains.
    pub fn release(mut self) -> Result<Vec<WaiterId>, LedgerError> {
        let Some(state) = self.state.upgrade() else {
            self.active = false;
            return Err(LedgerError::BrokerDropped);
        };
        let ready = state
            .borrow_mut()
            .release_grant(self.grant.id, self.grant.owner)?;
        self.active = false;
        Ok(ready)
    }
}

impl Drop for OwnedResourceLease {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(state) = self.state.upgrade() {
            let _result = state
                .borrow_mut()
                .release_grant(self.grant.id, self.grant.owner);
        }
        self.active = false;
    }
}

/// A non-cloneable queued request. Drop cancels the exact queue/ready identity.
#[derive(Debug)]
#[must_use = "dropping the waiter cancels its exact queued identity"]
pub struct OwnedResourceWaiter {
    state: Weak<RefCell<OwnedResourceState>>,
    waiter_id: WaiterId,
    active: bool,
}

impl OwnedResourceWaiter {
    #[must_use]
    pub fn id(&self) -> WaiterId {
        self.waiter_id
    }

    /// Acquire the grant once this exact FIFO waiter has become ready.
    ///
    /// # Errors
    ///
    /// Returns `WaiterAlreadyResolved` after cancellation/acquisition and
    /// `BrokerDropped` when no ledger remains.
    pub fn try_acquire(&mut self) -> Result<Option<OwnedResourceLease>, LedgerError> {
        if !self.active {
            return Err(LedgerError::WaiterAlreadyResolved(self.waiter_id));
        }
        let Some(state) = self.state.upgrade() else {
            self.active = false;
            return Err(LedgerError::BrokerDropped);
        };
        let Some(grant) = state.borrow_mut().ready.remove(&self.waiter_id) else {
            return Ok(None);
        };
        self.active = false;
        Ok(Some(OwnedResourceLease {
            state: Rc::downgrade(&state),
            grant,
            active: true,
        }))
    }

    /// Cancel this exact waiter and report later FIFO waiters made ready.
    ///
    /// # Errors
    ///
    /// Returns an identity/invariant error or `BrokerDropped`.
    pub fn cancel(mut self) -> Result<Vec<WaiterId>, LedgerError> {
        let Some(state) = self.state.upgrade() else {
            self.active = false;
            return Err(LedgerError::BrokerDropped);
        };
        let ready = state.borrow_mut().cancel_waiter(self.waiter_id)?;
        self.active = false;
        Ok(ready)
    }
}

impl Drop for OwnedResourceWaiter {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(state) = self.state.upgrade() {
            let _result = state.borrow_mut().cancel_waiter(self.waiter_id);
        }
        self.active = false;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreditLimitScope {
    Global,
    Owner,
    Stage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreditPressure {
    pub scope: CreditLimitScope,
    pub stage: ByteCreditStage,
    pub requested_items: u64,
    pub requested_bytes: u64,
    pub occupied_items: u64,
    pub occupied_bytes: u64,
    pub limit_items: u64,
    pub limit_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreditError {
    InvalidConfiguration,
    ZeroClaim,
    Backpressured(CreditPressure),
    SpillRequired(CreditPressure),
    RequiredLossRejected {
        claim_id: u64,
        component: ByteCreditComponent,
        stage: ByteCreditStage,
        bytes: u64,
    },
    ArithmeticOverflow,
    IdExhausted,
    UnknownClaim(u64),
    ClaimAlreadyReleased(u64),
    UnknownComponent {
        claim_id: u64,
        component: ByteCreditComponent,
    },
    ConsumedComponentCannotTransfer {
        component: ByteCreditComponent,
        consumed: u64,
    },
    UncreditedBytes {
        component: ByteCreditComponent,
        requested: u64,
        available: u64,
    },
    PositionRegressed,
    ReceivedAheadOfConsumed,
    WrittenAheadOfConsumed,
    WrittenAheadOfReceived,
    DurableAheadOfWritten,
    AcknowledgementBrokerMismatch,
    BrokerDropped,
    InvariantViolation,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ByteCreditOccupancy {
    pub global_items: u64,
    pub global_bytes: u64,
    pub owner_items: u64,
    pub owner_bytes: u64,
    pub stage_items: u64,
    pub stage_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ByteComponentClaim {
    owner: OwnerId,
    stage: ByteCreditStage,
    bytes: u64,
    consumed: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ByteClaim {
    components: BTreeMap<ByteCreditComponent, ByteComponentClaim>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CreditComponentAttribution {
    pub component: ByteCreditComponent,
    pub owner: OwnerId,
    pub stage: ByteCreditStage,
    pub bytes: u64,
    pub consumed: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreditAttribution {
    pub claim_id: u64,
    pub components: Vec<CreditComponentAttribution>,
}

/// A consumed, broker-correlated acknowledgement created only by core effect code.
#[derive(Debug)]
#[must_use = "durability does not advance until this effect acknowledgement is consumed"]
pub struct DurabilityAcknowledgement {
    ledger: Weak<RefCell<ByteCreditLedger>>,
    next: ByteCreditPosition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ByteCreditLedger {
    contract: ByteCreditContractV1,
    in_use: u64,
    next_claim: u64,
    active: BTreeMap<u64, ByteClaim>,
    lifetime_consumed_by_stage: BTreeMap<ByteCreditStage, u64>,
    position: ByteCreditPosition,
}

impl ByteCreditLedger {
    fn from_contract(contract: &ByteCreditContractV1) -> Result<Self, CreditError> {
        contract
            .validate()
            .map_err(|_| CreditError::InvalidConfiguration)?;
        Ok(Self {
            contract: contract.clone(),
            in_use: 0,
            next_claim: 1,
            active: BTreeMap::new(),
            lifetime_consumed_by_stage: BTreeMap::new(),
            position: ByteCreditPosition::default(),
        })
    }

    fn stage_policy(
        &self,
        stage: ByteCreditStage,
    ) -> Result<fforager_contracts::ByteCreditStagePolicy, CreditError> {
        self.contract
            .stage_policies
            .iter()
            .find(|policy| policy.stage == stage)
            .copied()
            .ok_or(CreditError::InvalidConfiguration)
    }

    fn component_count(&self) -> Result<u64, CreditError> {
        self.active.values().try_fold(0_u64, |total, claim| {
            total
                .checked_add(
                    u64::try_from(claim.components.len())
                        .map_err(|_| CreditError::ArithmeticOverflow)?,
                )
                .ok_or(CreditError::ArithmeticOverflow)
        })
    }

    fn occupancy_for(
        &self,
        owner: OwnerId,
        stage: ByteCreditStage,
    ) -> Result<ByteCreditOccupancy, CreditError> {
        let mut occupancy = ByteCreditOccupancy {
            global_items: self.component_count()?,
            global_bytes: self.in_use,
            ..ByteCreditOccupancy::default()
        };
        for component in self
            .active
            .values()
            .flat_map(|claim| claim.components.values())
        {
            if component.owner == owner {
                occupancy.owner_items = occupancy
                    .owner_items
                    .checked_add(1)
                    .ok_or(CreditError::ArithmeticOverflow)?;
                occupancy.owner_bytes = occupancy
                    .owner_bytes
                    .checked_add(component.bytes)
                    .ok_or(CreditError::ArithmeticOverflow)?;
            }
            if component.stage == stage {
                occupancy.stage_items = occupancy
                    .stage_items
                    .checked_add(1)
                    .ok_or(CreditError::ArithmeticOverflow)?;
                occupancy.stage_bytes = occupancy
                    .stage_bytes
                    .checked_add(component.bytes)
                    .ok_or(CreditError::ArithmeticOverflow)?;
            }
        }
        Ok(occupancy)
    }

    fn pressure_error(
        &self,
        stage: ByteCreditStage,
        pressure: CreditPressure,
    ) -> Result<CreditError, CreditError> {
        Ok(match self.stage_policy(stage)?.saturation_policy {
            ByteCreditSaturationPolicy::Backpressured => CreditError::Backpressured(pressure),
            ByteCreditSaturationPolicy::SpillRequired => CreditError::SpillRequired(pressure),
        })
    }

    fn check_component_admission(
        &self,
        owner: OwnerId,
        stage: ByteCreditStage,
        requested_items: u64,
        requested_bytes: u64,
        prior_requested_items: u64,
        prior_requested_bytes: u64,
    ) -> Result<(), CreditError> {
        let occupancy = self.occupancy_for(owner, stage)?;
        let global_items = occupancy
            .global_items
            .checked_add(prior_requested_items)
            .and_then(|value| value.checked_add(requested_items))
            .ok_or(CreditError::ArithmeticOverflow)?;
        let global_bytes = occupancy
            .global_bytes
            .checked_add(prior_requested_bytes)
            .and_then(|value| value.checked_add(requested_bytes))
            .ok_or(CreditError::ArithmeticOverflow)?;
        if global_items > self.contract.max_claim_items
            || global_bytes > self.contract.capacity_bytes
        {
            let occupied_items = occupancy
                .global_items
                .checked_add(prior_requested_items)
                .ok_or(CreditError::ArithmeticOverflow)?;
            let occupied_bytes = occupancy
                .global_bytes
                .checked_add(prior_requested_bytes)
                .ok_or(CreditError::ArithmeticOverflow)?;
            return Err(self.pressure_error(
                stage,
                CreditPressure {
                    scope: CreditLimitScope::Global,
                    stage,
                    requested_items,
                    requested_bytes,
                    occupied_items,
                    occupied_bytes,
                    limit_items: self.contract.max_claim_items,
                    limit_bytes: self.contract.capacity_bytes,
                },
            )?);
        }
        let owner_items = occupancy
            .owner_items
            .checked_add(prior_requested_items)
            .and_then(|value| value.checked_add(requested_items))
            .ok_or(CreditError::ArithmeticOverflow)?;
        let owner_bytes = occupancy
            .owner_bytes
            .checked_add(prior_requested_bytes)
            .and_then(|value| value.checked_add(requested_bytes))
            .ok_or(CreditError::ArithmeticOverflow)?;
        if owner_items > self.contract.max_owner_claim_items
            || owner_bytes > self.contract.max_owner_bytes
        {
            let occupied_items = occupancy
                .owner_items
                .checked_add(prior_requested_items)
                .ok_or(CreditError::ArithmeticOverflow)?;
            let occupied_bytes = occupancy
                .owner_bytes
                .checked_add(prior_requested_bytes)
                .ok_or(CreditError::ArithmeticOverflow)?;
            return Err(self.pressure_error(
                stage,
                CreditPressure {
                    scope: CreditLimitScope::Owner,
                    stage,
                    requested_items,
                    requested_bytes,
                    occupied_items,
                    occupied_bytes,
                    limit_items: self.contract.max_owner_claim_items,
                    limit_bytes: self.contract.max_owner_bytes,
                },
            )?);
        }
        let policy = self.stage_policy(stage)?;
        let stage_items = occupancy
            .stage_items
            .checked_add(requested_items)
            .ok_or(CreditError::ArithmeticOverflow)?;
        let stage_bytes = occupancy
            .stage_bytes
            .checked_add(requested_bytes)
            .ok_or(CreditError::ArithmeticOverflow)?;
        if stage_items > policy.max_claim_items || stage_bytes > policy.max_bytes {
            return Err(self.pressure_error(
                stage,
                CreditPressure {
                    scope: CreditLimitScope::Stage,
                    stage,
                    requested_items,
                    requested_bytes,
                    occupied_items: occupancy.stage_items,
                    occupied_bytes: occupancy.stage_bytes,
                    limit_items: policy.max_claim_items,
                    limit_bytes: policy.max_bytes,
                },
            )?);
        }
        Ok(())
    }

    fn claim_single(
        &mut self,
        owner: OwnerId,
        stage: ByteCreditStage,
        bytes: u64,
    ) -> Result<u64, CreditError> {
        if bytes == 0 {
            return Err(CreditError::ZeroClaim);
        }
        self.check_component_admission(owner, stage, 1, bytes, 0, 0)?;
        let mut components = BTreeMap::new();
        components.insert(
            ByteCreditComponent::Single,
            ByteComponentClaim {
                owner,
                stage,
                bytes,
                consumed: 0,
            },
        );
        self.insert_claim(components, bytes)
    }

    fn claim_coupled(
        &mut self,
        owner: OwnerId,
        reservation: CoupledByteReservation,
    ) -> Result<u64, CreditError> {
        let total = reservation.total_bytes().map_err(|error| match error {
            CoupledByteReservationError::ZeroInput | CoupledByteReservationError::ZeroOutput => {
                CreditError::ZeroClaim
            }
            CoupledByteReservationError::ArithmeticOverflow => CreditError::ArithmeticOverflow,
        })?;
        self.check_component_admission(
            owner,
            reservation.input_stage,
            1,
            reservation.input_bytes,
            0,
            0,
        )?;
        let same_stage = reservation.output_stage == reservation.input_stage;
        self.check_component_admission(
            owner,
            reservation.output_stage,
            1,
            reservation.output_bytes,
            1,
            reservation.input_bytes,
        )?;
        if same_stage {
            let occupancy = self.occupancy_for(owner, reservation.input_stage)?;
            let policy = self.stage_policy(reservation.input_stage)?;
            if occupancy
                .stage_items
                .checked_add(2)
                .ok_or(CreditError::ArithmeticOverflow)?
                > policy.max_claim_items
                || occupancy
                    .stage_bytes
                    .checked_add(total)
                    .ok_or(CreditError::ArithmeticOverflow)?
                    > policy.max_bytes
            {
                return Err(self.pressure_error(
                    reservation.input_stage,
                    CreditPressure {
                        scope: CreditLimitScope::Stage,
                        stage: reservation.input_stage,
                        requested_items: 2,
                        requested_bytes: total,
                        occupied_items: occupancy.stage_items,
                        occupied_bytes: occupancy.stage_bytes,
                        limit_items: policy.max_claim_items,
                        limit_bytes: policy.max_bytes,
                    },
                )?);
            }
        }
        let mut components = BTreeMap::new();
        components.insert(
            ByteCreditComponent::Input,
            ByteComponentClaim {
                owner,
                stage: reservation.input_stage,
                bytes: reservation.input_bytes,
                consumed: 0,
            },
        );
        components.insert(
            ByteCreditComponent::Output,
            ByteComponentClaim {
                owner,
                stage: reservation.output_stage,
                bytes: reservation.output_bytes,
                consumed: 0,
            },
        );
        self.insert_claim(components, total)
    }

    fn insert_claim(
        &mut self,
        components: BTreeMap<ByteCreditComponent, ByteComponentClaim>,
        bytes: u64,
    ) -> Result<u64, CreditError> {
        let combined = self
            .in_use
            .checked_add(bytes)
            .ok_or(CreditError::ArithmeticOverflow)?;
        let id = self.next_claim;
        let next_claim = id.checked_add(1).ok_or(CreditError::IdExhausted)?;
        self.active.insert(id, ByteClaim { components });
        self.in_use = combined;
        self.next_claim = next_claim;
        Ok(id)
    }

    fn transfer_component(
        &mut self,
        id: u64,
        component: ByteCreditComponent,
        to: OwnerId,
    ) -> Result<(), CreditError> {
        let mut candidate = self.clone();
        let Some(existing) = candidate
            .active
            .get(&id)
            .and_then(|claim| claim.components.get(&component))
            .copied()
        else {
            return candidate.missing_component(id, component);
        };
        if existing.consumed != 0 {
            return Err(CreditError::ConsumedComponentCannotTransfer {
                component,
                consumed: existing.consumed,
            });
        }
        if existing.owner != to {
            candidate.check_owner_transfer(to, existing)?;
            let Some(claim) = candidate.active.get_mut(&id) else {
                return Err(CreditError::InvariantViolation);
            };
            let Some(target) = claim.components.get_mut(&component) else {
                return Err(CreditError::InvariantViolation);
            };
            target.owner = to;
        }
        *self = candidate;
        Ok(())
    }

    fn check_owner_transfer(
        &self,
        to: OwnerId,
        component: ByteComponentClaim,
    ) -> Result<(), CreditError> {
        let occupancy = self.occupancy_for(to, component.stage)?;
        if occupancy
            .owner_items
            .checked_add(1)
            .ok_or(CreditError::ArithmeticOverflow)?
            > self.contract.max_owner_claim_items
            || occupancy
                .owner_bytes
                .checked_add(component.bytes)
                .ok_or(CreditError::ArithmeticOverflow)?
                > self.contract.max_owner_bytes
        {
            return Err(self.pressure_error(
                component.stage,
                CreditPressure {
                    scope: CreditLimitScope::Owner,
                    stage: component.stage,
                    requested_items: 1,
                    requested_bytes: component.bytes,
                    occupied_items: occupancy.owner_items,
                    occupied_bytes: occupancy.owner_bytes,
                    limit_items: self.contract.max_owner_claim_items,
                    limit_bytes: self.contract.max_owner_bytes,
                },
            )?);
        }
        Ok(())
    }

    fn consume_component(
        &mut self,
        id: u64,
        component: ByteCreditComponent,
        bytes: u64,
    ) -> Result<(), CreditError> {
        let mut candidate = self.clone();
        let Some(target) = candidate
            .active
            .get_mut(&id)
            .and_then(|claim| claim.components.get_mut(&component))
        else {
            return candidate.missing_component(id, component);
        };
        let available = target
            .bytes
            .checked_sub(target.consumed)
            .ok_or(CreditError::InvariantViolation)?;
        if bytes > available {
            return Err(CreditError::UncreditedBytes {
                component,
                requested: bytes,
                available,
            });
        }
        target.consumed = target
            .consumed
            .checked_add(bytes)
            .ok_or(CreditError::ArithmeticOverflow)?;
        let stage = target.stage;
        let stage_consumed = candidate
            .lifetime_consumed_by_stage
            .get(&stage)
            .copied()
            .unwrap_or(0)
            .checked_add(bytes)
            .ok_or(CreditError::ArithmeticOverflow)?;
        candidate
            .lifetime_consumed_by_stage
            .insert(stage, stage_consumed);
        *self = candidate;
        Ok(())
    }

    fn release(&mut self, id: u64) -> Result<CreditAttribution, CreditError> {
        let mut candidate = self.clone();
        let attribution = candidate.release_checked(id)?;
        *self = candidate;
        Ok(attribution)
    }

    fn release_checked(&mut self, id: u64) -> Result<CreditAttribution, CreditError> {
        let Some(claim) = self.active.remove(&id) else {
            return self.missing_claim(id);
        };
        let bytes = claim
            .components
            .values()
            .try_fold(0_u64, |total, component| {
                total
                    .checked_add(component.bytes)
                    .ok_or(CreditError::ArithmeticOverflow)
            })?;
        self.in_use = self
            .in_use
            .checked_sub(bytes)
            .ok_or(CreditError::InvariantViolation)?;
        Ok(Self::attribution_from_claim(id, &claim))
    }

    fn attribution(&self, id: u64) -> Result<CreditAttribution, CreditError> {
        let Some(claim) = self.active.get(&id) else {
            return self.missing_claim(id);
        };
        Ok(Self::attribution_from_claim(id, claim))
    }

    fn attribution_from_claim(id: u64, claim: &ByteClaim) -> CreditAttribution {
        CreditAttribution {
            claim_id: id,
            components: claim
                .components
                .iter()
                .map(|(component, claim)| CreditComponentAttribution {
                    component: *component,
                    owner: claim.owner,
                    stage: claim.stage,
                    bytes: claim.bytes,
                    consumed: claim.consumed,
                })
                .collect(),
        }
    }

    fn validate_next_position(&self, next: ByteCreditPosition) -> Result<(), CreditError> {
        if next.received < self.position.received
            || next.validated_written_contiguous < self.position.validated_written_contiguous
            || next.durable_contiguous < self.position.durable_contiguous
        {
            return Err(CreditError::PositionRegressed);
        }
        if next.received > self.consumed_for_stage(ByteCreditStage::HttpReceive) {
            return Err(CreditError::ReceivedAheadOfConsumed);
        }
        if next.validated_written_contiguous > self.consumed_for_stage(ByteCreditStage::Writer) {
            return Err(CreditError::WrittenAheadOfConsumed);
        }
        if next.validated_written_contiguous > next.received {
            return Err(CreditError::WrittenAheadOfReceived);
        }
        if next.durable_contiguous > next.validated_written_contiguous {
            return Err(CreditError::DurableAheadOfWritten);
        }
        Ok(())
    }

    fn consumed_for_stage(&self, stage: ByteCreditStage) -> u64 {
        self.lifetime_consumed_by_stage
            .get(&stage)
            .copied()
            .unwrap_or(0)
    }

    fn advance_acknowledged(&mut self, next: ByteCreditPosition) -> Result<(), CreditError> {
        self.validate_next_position(next)?;
        self.position = next;
        Ok(())
    }

    fn verify(&self) -> Result<(), CreditError> {
        let total = self.active.values().try_fold(0_u64, |sum, claim| {
            claim.components.values().try_fold(sum, |sum, component| {
                if component.consumed > component.bytes {
                    return Err(CreditError::InvariantViolation);
                }
                sum.checked_add(component.bytes)
                    .ok_or(CreditError::ArithmeticOverflow)
            })
        })?;
        if total != self.in_use || self.in_use > self.contract.capacity_bytes {
            return Err(CreditError::InvariantViolation);
        }
        self.validate_next_position(self.position)?;
        for stage in self.contract.stages {
            let policy = self.stage_policy(stage)?;
            let occupancy = self.occupancy_for(OwnerId(u64::MAX), stage)?;
            if occupancy.stage_items > policy.max_claim_items
                || occupancy.stage_bytes > policy.max_bytes
            {
                return Err(CreditError::InvariantViolation);
            }
        }
        for owner in self
            .active
            .values()
            .flat_map(|claim| claim.components.values().map(|component| component.owner))
            .collect::<BTreeSet<_>>()
        {
            let occupancy = self.occupancy_for(owner, ByteCreditStage::HttpReceive)?;
            if occupancy.owner_items > self.contract.max_owner_claim_items
                || occupancy.owner_bytes > self.contract.max_owner_bytes
            {
                return Err(CreditError::InvariantViolation);
            }
        }
        Ok(())
    }

    fn missing_claim<T>(&self, id: u64) -> Result<T, CreditError> {
        if id > 0 && id < self.next_claim {
            Err(CreditError::ClaimAlreadyReleased(id))
        } else {
            Err(CreditError::UnknownClaim(id))
        }
    }

    fn missing_component<T>(
        &self,
        id: u64,
        component: ByteCreditComponent,
    ) -> Result<T, CreditError> {
        if self.active.contains_key(&id) {
            Err(CreditError::UnknownComponent {
                claim_id: id,
                component,
            })
        } else {
            self.missing_claim(id)
        }
    }
}

/// Single-threaded owned boundary for validated byte-credit contracts.
#[derive(Clone, Debug)]
pub struct OwnedByteCreditBroker {
    ledger: Rc<RefCell<ByteCreditLedger>>,
}

impl OwnedByteCreditBroker {
    /// Build the owned broker from a validated closed byte-credit contract.
    ///
    /// # Errors
    ///
    /// Returns `InvalidConfiguration` when the contract is not the exact valid v1 profile.
    pub fn from_contract(contract: &ByteCreditContractV1) -> Result<Self, CreditError> {
        Ok(Self {
            ledger: Rc::new(RefCell::new(ByteCreditLedger::from_contract(contract)?)),
        })
    }

    /// Claim one stage-bound byte component for an owner.
    ///
    /// # Errors
    ///
    /// Returns a typed pressure, arithmetic, identity, or configuration error.
    pub fn claim(
        &self,
        owner: OwnerId,
        stage: ByteCreditStage,
        bytes: u64,
    ) -> Result<OwnedByteCreditLease, CreditError> {
        let claim_id = self.ledger.borrow_mut().claim_single(owner, stage, bytes)?;
        Ok(OwnedByteCreditLease {
            ledger: Rc::downgrade(&self.ledger),
            claim_id,
            active: true,
        })
    }

    /// Claim linked input and output components atomically.
    ///
    /// # Errors
    ///
    /// Returns a typed pressure or reservation error without partially claiming either component.
    pub fn claim_coupled(
        &self,
        owner: OwnerId,
        reservation: CoupledByteReservation,
    ) -> Result<OwnedByteCreditLease, CreditError> {
        let claim_id = self.ledger.borrow_mut().claim_coupled(owner, reservation)?;
        Ok(OwnedByteCreditLease {
            ledger: Rc::downgrade(&self.ledger),
            claim_id,
            active: true,
        })
    }

    /// Return exact global component and byte occupancy.
    ///
    /// # Errors
    ///
    /// Returns `ArithmeticOverflow` if the internal component count cannot be represented.
    pub fn global_occupancy(&self) -> Result<(u64, u64), CreditError> {
        let ledger = self.ledger.borrow();
        Ok((ledger.component_count()?, ledger.in_use))
    }

    /// Return global, owner, and stage occupancy in one snapshot.
    ///
    /// # Errors
    ///
    /// Returns `ArithmeticOverflow` if exact occupancy cannot be represented.
    pub fn occupancy(
        &self,
        owner: OwnerId,
        stage: ByteCreditStage,
    ) -> Result<ByteCreditOccupancy, CreditError> {
        self.ledger.borrow().occupancy_for(owner, stage)
    }

    #[must_use]
    pub fn position(&self) -> ByteCreditPosition {
        self.ledger.borrow().position
    }

    pub(crate) fn acknowledge_durability_effects(
        &self,
        next: ByteCreditPosition,
    ) -> Result<DurabilityAcknowledgement, CreditError> {
        self.ledger.borrow().validate_next_position(next)?;
        Ok(DurabilityAcknowledgement {
            ledger: Rc::downgrade(&self.ledger),
            next,
        })
    }

    /// Consume an effect-produced acknowledgement and advance its exact broker.
    ///
    /// # Errors
    ///
    /// Returns a broker-identity or durability-ordering error without advancing state.
    pub fn advance(&self, acknowledgement: DurabilityAcknowledgement) -> Result<(), CreditError> {
        let DurabilityAcknowledgement { ledger, next } = acknowledgement;
        let Some(acknowledged_ledger) = ledger.upgrade() else {
            return Err(CreditError::BrokerDropped);
        };
        if !Rc::ptr_eq(&acknowledged_ledger, &self.ledger) {
            return Err(CreditError::AcknowledgementBrokerMismatch);
        }
        self.ledger.borrow_mut().advance_acknowledged(next)
    }

    /// Verify all global, owner, stage, attribution, and durability invariants.
    ///
    /// # Errors
    ///
    /// Returns a typed invariant, arithmetic, configuration, or position error.
    pub fn verify(&self) -> Result<(), CreditError> {
        self.ledger.borrow().verify()
    }
}

/// A non-cloneable owned byte claim. Drop releases every linked component once.
#[derive(Debug)]
#[must_use = "dropping the lease releases its complete linked reservation"]
pub struct OwnedByteCreditLease {
    ledger: Weak<RefCell<ByteCreditLedger>>,
    claim_id: u64,
    active: bool,
}

impl OwnedByteCreditLease {
    #[must_use]
    pub fn claim_id(&self) -> u64 {
        self.claim_id
    }

    /// Transfer one exact unconsumed linked component to another owner.
    ///
    /// # Errors
    ///
    /// Returns a typed identity, pressure, consumed-component, or broker error atomically.
    pub fn transfer_component(
        &mut self,
        component: ByteCreditComponent,
        to: OwnerId,
    ) -> Result<(), CreditError> {
        let Some(ledger) = self.ledger.upgrade() else {
            return Err(CreditError::BrokerDropped);
        };
        ledger
            .borrow_mut()
            .transfer_component(self.claim_id, component, to)
    }

    /// Attribute consumed bytes to one exact component.
    ///
    /// # Errors
    ///
    /// Returns a typed identity, capacity, arithmetic, or broker error atomically.
    pub fn consume(
        &mut self,
        component: ByteCreditComponent,
        bytes: u64,
    ) -> Result<(), CreditError> {
        let Some(ledger) = self.ledger.upgrade() else {
            return Err(CreditError::BrokerDropped);
        };
        ledger
            .borrow_mut()
            .consume_component(self.claim_id, component, bytes)
    }

    /// Reject any attempted loss of required credited bytes with stage attribution.
    ///
    /// # Errors
    ///
    /// Returns `RequiredLossRejected` for non-zero loss, or an identity/broker error.
    pub fn attempt_required_loss(
        &self,
        component: ByteCreditComponent,
        bytes: u64,
    ) -> Result<(), CreditError> {
        if bytes == 0 {
            return Ok(());
        }
        let Some(ledger) = self.ledger.upgrade() else {
            return Err(CreditError::BrokerDropped);
        };
        let attribution = ledger.borrow().attribution(self.claim_id)?;
        let Some(component_attribution) = attribution
            .components
            .iter()
            .find(|attribution| attribution.component == component)
        else {
            return Err(CreditError::UnknownComponent {
                claim_id: self.claim_id,
                component,
            });
        };
        Err(CreditError::RequiredLossRejected {
            claim_id: self.claim_id,
            component,
            stage: component_attribution.stage,
            bytes,
        })
    }

    /// Snapshot the immutable component identities and current ownership/consumption.
    ///
    /// # Errors
    ///
    /// Returns an identity or broker error when the live claim cannot be resolved.
    pub fn attribution(&self) -> Result<CreditAttribution, CreditError> {
        let Some(ledger) = self.ledger.upgrade() else {
            return Err(CreditError::BrokerDropped);
        };
        ledger.borrow().attribution(self.claim_id)
    }

    /// Explicitly release every component in this linked claim exactly once.
    ///
    /// # Errors
    ///
    /// Returns an identity, invariant, arithmetic, or broker error.
    pub fn release(mut self) -> Result<CreditAttribution, CreditError> {
        let Some(ledger) = self.ledger.upgrade() else {
            self.active = false;
            return Err(CreditError::BrokerDropped);
        };
        let attribution = ledger.borrow_mut().release(self.claim_id)?;
        self.active = false;
        Ok(attribution)
    }

    /// Cancel every component in this linked claim exactly once.
    ///
    /// # Errors
    ///
    /// Returns the same typed errors as explicit release.
    pub fn cancel(self) -> Result<CreditAttribution, CreditError> {
        self.release()
    }
}

impl Drop for OwnedByteCreditLease {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        if let Some(ledger) = self.ledger.upgrade() {
            let _result = ledger.borrow_mut().release(self.claim_id);
        }
        self.active = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector(memory: u64, handles: u32, processes: u32) -> ResourceVector {
        ResourceVector {
            memory_bytes: memory,
            open_handles: handles,
            ffmpeg_processes: processes,
            ..ResourceVector::default()
        }
    }

    #[test]
    fn atomic_zero_exact_one_over_and_release_identity() -> Result<(), String> {
        let mut ledger = ResourceLedger::new(vector(10, 2, 1), 2, 2, 20);
        let zero = ledger.request(OwnerId(1), vector(0, 0, 0));
        assert!(matches!(zero, Ok(Admission::Granted(_))));
        let exact = ledger.request(OwnerId(2), vector(10, 2, 1));
        let Ok(Admission::Granted(exact)) = exact else {
            return Err("exact capacity must be granted".to_owned());
        };
        assert!(matches!(
            ledger.request(OwnerId(3), vector(11, 0, 0)),
            Err(LedgerError::RequestExceedsCapacity)
        ));
        assert!(matches!(
            ledger.release(exact.id, OwnerId(9)),
            Err(LedgerError::GrantOwnerMismatch { .. })
        ));
        assert!(ledger.release(exact.id, exact.owner).is_ok());
        assert!(matches!(
            ledger.release(exact.id, exact.owner),
            Err(LedgerError::GrantAlreadyReleased(_))
        ));
        assert!(ledger.verify().is_ok());
        Ok(())
    }

    #[test]
    fn checked_vector_never_wraps_or_underflows() {
        assert!(
            vector(u64::MAX, u32::MAX, u32::MAX)
                .checked_add(vector(1, 1, 1))
                .is_none()
        );
        assert!(
            ResourceVector::default()
                .checked_sub(vector(1, 0, 0))
                .is_none()
        );
    }

    #[test]
    fn queue_is_bounded_fifo_and_cancelled_waiters_progress() -> Result<(), String> {
        let mut ledger = ResourceLedger::new(vector(10, 2, 1), 2, 2, 20);
        let first = ledger.request(OwnerId(1), vector(10, 1, 0));
        let Ok(Admission::Granted(first)) = first else {
            return Err("initial grant required".to_owned());
        };
        let large = ledger.request(OwnerId(2), vector(10, 1, 0));
        let Ok(Admission::Queued(large)) = large else {
            return Err("large request must queue".to_owned());
        };
        let small = ledger.request(OwnerId(3), vector(1, 1, 0));
        let Ok(Admission::Queued(small)) = small else {
            return Err("small request must queue behind FIFO head".to_owned());
        };
        assert!(matches!(
            ledger.request(OwnerId(4), vector(1, 0, 0)),
            Err(LedgerError::QueueItemLimit)
        ));
        assert!(ledger.cancel_waiter(large).is_ok());
        let issued = ledger.release(first.id, first.owner);
        let Ok(issued) = issued else {
            return Err("release must dispatch the next waiter".to_owned());
        };
        assert_eq!(issued.len(), 1);
        assert_eq!(issued.first().map(|grant| grant.owner), Some(OwnerId(3)));
        assert!(matches!(
            ledger.cancel_waiter(small),
            Err(LedgerError::UnknownWaiter(_))
        ));
        assert!(ledger.verify().is_ok());
        Ok(())
    }

    #[test]
    fn queue_byte_bound_rejects_saturation_without_mutation() {
        let mut ledger = ResourceLedger::new(vector(10, 1, 0), 2, 4, 5);
        let first = ledger.request(OwnerId(1), vector(10, 0, 0));
        assert!(matches!(first, Ok(Admission::Granted(_))));
        assert!(matches!(
            ledger.request(OwnerId(2), vector(6, 0, 0)),
            Err(LedgerError::QueueByteLimit)
        ));
        assert_eq!(ledger.waiter_occupancy(), (0, 0));
    }

    #[test]
    // WP009-REG-BYTE-STAGE-BOUNDARY-001
    fn byte_credit_stage_and_owner_bounds_return_closed_pressure_outcomes() {
        let mut contract = ByteCreditContractV1::new(20, 8);
        contract.max_owner_bytes = 6;
        contract.max_owner_claim_items = 2;
        for policy in &mut contract.stage_policies {
            policy.max_bytes = 10;
            policy.max_claim_items = 4;
            if matches!(
                policy.stage,
                ByteCreditStage::HttpReceive | ByteCreditStage::SpillAndSink
            ) {
                policy.max_bytes = 4;
                policy.max_claim_items = 1;
            }
        }
        assert!(contract.validate().is_ok());
        assert_eq!(
            contract
                .stage_policies
                .iter()
                .map(|policy| policy.stage)
                .collect::<Vec<_>>(),
            contract.stages
        );

        let broker = OwnedByteCreditBroker::from_contract(&contract).expect("valid contract");
        let http = broker
            .claim(OwnerId(1), ByteCreditStage::HttpReceive, 4)
            .expect("exact stage bound");
        assert!(matches!(
            broker.claim(OwnerId(2), ByteCreditStage::HttpReceive, 1),
            Err(CreditError::Backpressured(CreditPressure {
                scope: CreditLimitScope::Stage,
                stage: ByteCreditStage::HttpReceive,
                occupied_items: 1,
                occupied_bytes: 4,
                limit_items: 1,
                limit_bytes: 4,
                ..
            }))
        ));
        assert!(matches!(
            broker.claim(OwnerId(1), ByteCreditStage::Decompression, 3),
            Err(CreditError::Backpressured(CreditPressure {
                scope: CreditLimitScope::Owner,
                occupied_items: 1,
                occupied_bytes: 4,
                limit_items: 2,
                limit_bytes: 6,
                ..
            }))
        ));
        assert!(matches!(
            broker.claim(OwnerId(2), ByteCreditStage::SpillAndSink, 5),
            Err(CreditError::SpillRequired(CreditPressure {
                scope: CreditLimitScope::Stage,
                stage: ByteCreditStage::SpillAndSink,
                limit_items: 1,
                limit_bytes: 4,
                ..
            }))
        ));
        let occupancy = broker
            .occupancy(OwnerId(1), ByteCreditStage::HttpReceive)
            .expect("bounded occupancy");
        assert_eq!(
            occupancy,
            ByteCreditOccupancy {
                global_items: 1,
                global_bytes: 4,
                owner_items: 1,
                owner_bytes: 4,
                stage_items: 1,
                stage_bytes: 4,
            }
        );
        assert!(matches!(
            http.attempt_required_loss(ByteCreditComponent::Single, 1),
            Err(CreditError::RequiredLossRejected {
                stage: ByteCreditStage::HttpReceive,
                bytes: 1,
                ..
            })
        ));
        assert_eq!(broker.global_occupancy(), Ok((1, 4)));
        drop(http);
        assert_eq!(broker.global_occupancy(), Ok((0, 0)));
        assert!(broker.verify().is_ok());
    }

    #[test]
    fn receive_requires_exact_claim_owner_and_records_attribution() {
        let broker = OwnedByteCreditBroker::from_contract(&ByteCreditContractV1::new(10, 2))
            .expect("valid byte-credit contract");
        let first = broker
            .claim(OwnerId(1), ByteCreditStage::HttpReceive, 5)
            .expect("first owned claim");
        let mut second = broker
            .claim(OwnerId(2), ByteCreditStage::HttpReceive, 5)
            .expect("second owned claim");

        second
            .consume(ByteCreditComponent::Single, 3)
            .expect("receive is attributed through the exact owned claim");
        let first_attribution = first.attribution().expect("first attribution");
        assert_eq!(first_attribution.components.len(), 1);
        assert_eq!(
            first_attribution.components[0],
            CreditComponentAttribution {
                component: ByteCreditComponent::Single,
                owner: OwnerId(1),
                stage: ByteCreditStage::HttpReceive,
                bytes: 5,
                consumed: 0,
            }
        );
        let consumed_attribution = second.attribution().expect("second attribution");
        assert_eq!(consumed_attribution.components.len(), 1);
        assert_eq!(
            consumed_attribution.components[0],
            CreditComponentAttribution {
                component: ByteCreditComponent::Single,
                owner: OwnerId(2),
                stage: ByteCreditStage::HttpReceive,
                bytes: 5,
                consumed: 3,
            }
        );

        assert_eq!(
            second.transfer_component(ByteCreditComponent::Single, OwnerId(1)),
            Err(CreditError::ConsumedComponentCannotTransfer {
                component: ByteCreditComponent::Single,
                consumed: 3,
            })
        );
        assert_eq!(
            second.attribution(),
            Ok(consumed_attribution),
            "rejected owner transfer must not rewrite receive attribution"
        );
        assert!(broker.verify().is_ok());
    }

    #[test]
    // WP009-REG-COUPLED-TRANSFER-001
    fn coupled_components_keep_stage_identity_and_transfer_only_unconsumed_output() {
        let contract = ByteCreditContractV1::new(10, 2);
        let broker = OwnedByteCreditBroker::from_contract(&contract).expect("valid contract");
        let mut lease = broker
            .claim_coupled(
                OwnerId(1),
                CoupledByteReservation {
                    input_stage: ByteCreditStage::DecryptOrPackInput,
                    input_bytes: 4,
                    output_stage: ByteCreditStage::DecryptOrPackOutput,
                    output_bytes: 6,
                },
            )
            .expect("coupled exact-capacity claim");
        lease
            .consume(ByteCreditComponent::Input, 4)
            .expect("consume credited input");
        lease
            .transfer_component(ByteCreditComponent::Output, OwnerId(2))
            .expect("unconsumed output transfer");
        assert_eq!(
            lease.transfer_component(ByteCreditComponent::Input, OwnerId(2)),
            Err(CreditError::ConsumedComponentCannotTransfer {
                component: ByteCreditComponent::Input,
                consumed: 4,
            })
        );
        let attribution = lease.attribution().expect("linked attribution");
        assert_eq!(attribution.components.len(), 2);
        assert_eq!(
            attribution.components[0],
            CreditComponentAttribution {
                component: ByteCreditComponent::Input,
                owner: OwnerId(1),
                stage: ByteCreditStage::DecryptOrPackInput,
                bytes: 4,
                consumed: 4,
            }
        );
        assert_eq!(
            attribution.components[1],
            CreditComponentAttribution {
                component: ByteCreditComponent::Output,
                owner: OwnerId(2),
                stage: ByteCreditStage::DecryptOrPackOutput,
                bytes: 6,
                consumed: 0,
            }
        );
        assert!(broker.verify().is_ok());
    }

    #[test]
    // WP009-REG-DURABLE-EFFECT-ACK-001
    fn durability_requires_consumed_bytes_and_exact_broker_effect_acknowledgement() {
        let contract = ByteCreditContractV1::new(8, 2);
        let broker = OwnedByteCreditBroker::from_contract(&contract).expect("valid contract");
        let other = OwnedByteCreditBroker::from_contract(&contract).expect("valid contract");
        let unrelated = OwnedByteCreditBroker::from_contract(&contract).expect("valid contract");
        let mut unrelated_lease = unrelated
            .claim(OwnerId(9), ByteCreditStage::Decompression, 8)
            .expect("unrelated-stage credit claim");
        unrelated_lease
            .consume(ByteCreditComponent::Single, 8)
            .expect("unrelated-stage consumption");
        drop(unrelated_lease);
        assert!(matches!(
            unrelated.acknowledge_durability_effects(ByteCreditPosition {
                received: 8,
                validated_written_contiguous: 0,
                durable_contiguous: 0,
            }),
            Err(CreditError::ReceivedAheadOfConsumed)
        ));

        let mut receive_lease = broker
            .claim(OwnerId(1), ByteCreditStage::HttpReceive, 8)
            .expect("receive-stage credit claim");
        receive_lease
            .consume(ByteCreditComponent::Single, 8)
            .expect("receive-stage effect consumption");
        drop(receive_lease);
        let mut writer_lease = broker
            .claim(OwnerId(1), ByteCreditStage::Writer, 8)
            .expect("writer-stage credit claim");
        writer_lease
            .consume(ByteCreditComponent::Single, 8)
            .expect("writer-stage effect consumption");
        drop(writer_lease);
        assert_eq!(broker.global_occupancy(), Ok((0, 0)));

        let next = ByteCreditPosition {
            received: 8,
            validated_written_contiguous: 7,
            durable_contiguous: 6,
        };
        let wrong_broker_token = broker
            .acknowledge_durability_effects(next)
            .expect("core effect acknowledgement");
        assert_eq!(
            other.advance(wrong_broker_token),
            Err(CreditError::AcknowledgementBrokerMismatch)
        );
        let token = broker
            .acknowledge_durability_effects(next)
            .expect("core effect acknowledgement");
        assert!(broker.advance(token).is_ok());
        assert_eq!(broker.position(), next);
        assert!(matches!(
            broker.acknowledge_durability_effects(ByteCreditPosition {
                received: 8,
                validated_written_contiguous: 6,
                durable_contiguous: 6,
            }),
            Err(CreditError::PositionRegressed)
        ));
        assert!(matches!(
            broker.acknowledge_durability_effects(ByteCreditPosition {
                received: 9,
                validated_written_contiguous: 7,
                durable_contiguous: 6,
            }),
            Err(CreditError::ReceivedAheadOfConsumed)
        ));
        assert!(matches!(
            broker.acknowledge_durability_effects(ByteCreditPosition {
                received: 8,
                validated_written_contiguous: 9,
                durable_contiguous: 6,
            }),
            Err(CreditError::WrittenAheadOfConsumed)
        ));
        assert!(broker.verify().is_ok());
    }

    #[test]
    fn release_is_atomic_when_a_waiter_cannot_fit_at_integer_boundary() -> Result<(), String> {
        let mut ledger = ResourceLedger::new(vector(u64::MAX, 0, 0), 3, 2, u64::MAX);
        let first = ledger.request(OwnerId(1), vector(u64::MAX - 10, 0, 0));
        let Ok(Admission::Granted(_first)) = first else {
            return Err("first boundary grant must be admitted".to_owned());
        };
        let second = ledger.request(OwnerId(2), vector(5, 0, 0));
        let Ok(Admission::Granted(second)) = second else {
            return Err("second boundary grant must be admitted".to_owned());
        };
        assert!(matches!(
            ledger.request(OwnerId(3), vector(20, 0, 0)),
            Ok(Admission::Queued(_))
        ));
        assert!(
            matches!(ledger.release(second.id, second.owner), Ok(ref issued) if issued.is_empty())
        );
        assert_eq!(ledger.in_use(), vector(u64::MAX - 10, 0, 0));
        assert!(ledger.verify().is_ok());
        Ok(())
    }

    #[test]
    fn active_and_claim_item_limits_bound_zero_and_tiny_ownership() {
        let mut ledger = ResourceLedger::new(ResourceVector::default(), 1, 1, 0);
        assert!(matches!(
            ledger.request(OwnerId(1), ResourceVector::default()),
            Ok(Admission::Granted(_))
        ));
        assert!(matches!(
            ledger.request(OwnerId(2), ResourceVector::default()),
            Ok(Admission::Queued(_))
        ));
        assert_eq!(ledger.waiter_occupancy(), (1, 0));

        let credits = OwnedByteCreditBroker::from_contract(&ByteCreditContractV1::new(2, 1))
            .expect("valid contract");
        let _lease = credits
            .claim(OwnerId(1), ByteCreditStage::HttpReceive, 1)
            .expect("first claim");
        assert!(matches!(
            credits.claim(OwnerId(2), ByteCreditStage::Decompression, 1),
            Err(CreditError::Backpressured(CreditPressure {
                scope: CreditLimitScope::Global,
                ..
            }))
        ));
    }

    #[test]
    fn fair_scheduler_preserves_fifo_when_head_becomes_eligible() -> Result<(), String> {
        let mut ledger = ResourceLedger::new(vector(10, 2, 0), 2, 2, 20);
        let first = ledger.request(OwnerId(1), vector(5, 0, 0));
        let Ok(Admission::Granted(first)) = first else {
            return Err("initial grant required".to_owned());
        };
        assert!(matches!(
            ledger.request(OwnerId(2), vector(10, 0, 0)),
            Ok(Admission::Queued(_))
        ));
        assert!(matches!(
            ledger.request(OwnerId(3), vector(1, 0, 0)),
            Ok(Admission::Queued(_))
        ));
        let issued = ledger.release(first.id, first.owner);
        assert!(matches!(
            issued,
            Ok(grants) if grants.first().map(|grant| grant.owner) == Some(OwnerId(2))
        ));
        assert_eq!(ledger.waiter_occupancy(), (1, 1));
        Ok(())
    }

    #[test]
    fn fifo_head_reservation_prevents_large_request_starvation() -> Result<(), String> {
        let mut ledger = ResourceLedger::new(vector(10, 0, 0), 3, 4, 40);
        let six = ledger.request(OwnerId(1), vector(6, 0, 0));
        let Ok(Admission::Granted(six)) = six else {
            return Err("six-unit grant required".to_owned());
        };
        let four = ledger.request(OwnerId(2), vector(4, 0, 0));
        let Ok(Admission::Granted(four)) = four else {
            return Err("four-unit grant required".to_owned());
        };
        assert!(matches!(
            ledger.request(OwnerId(3), vector(10, 0, 0)),
            Ok(Admission::Queued(_))
        ));
        assert!(matches!(
            ledger.request(OwnerId(4), vector(6, 0, 0)),
            Ok(Admission::Queued(_))
        ));

        let first_release = ledger
            .release(six.id, six.owner)
            .map_err(|error| format!("first release failed: {error:?}"))?;
        assert!(
            first_release.is_empty(),
            "later small work may not bypass FIFO head"
        );
        assert_eq!(ledger.in_use(), vector(4, 0, 0));

        let second_release = ledger
            .release(four.id, four.owner)
            .map_err(|error| format!("second release failed: {error:?}"))?;
        assert_eq!(second_release.len(), 1);
        assert_eq!(second_release[0].owner, OwnerId(3));
        assert_eq!(ledger.waiter_occupancy(), (1, 6));
        assert!(ledger.verify().is_ok());
        Ok(())
    }

    #[test]
    fn one_owner_cannot_monopolize_multiple_active_slots() {
        let mut ledger = ResourceLedger::new(vector(10, 0, 0), 2, 2, 20);
        assert!(matches!(
            ledger.request(OwnerId(1), vector(1, 0, 0)),
            Ok(Admission::Granted(_))
        ));
        let same_owner = ledger.request(OwnerId(1), vector(1, 0, 0));
        let Ok(Admission::Queued(same_owner)) = same_owner else {
            return assert!(matches!(same_owner, Ok(Admission::Queued(_))));
        };
        assert!(matches!(
            ledger.request(OwnerId(2), vector(1, 0, 0)),
            Ok(Admission::Queued(_))
        ));
        assert!(matches!(
            ledger.cancel_waiter(same_owner),
            Ok(grants) if grants.first().map(|grant| grant.owner) == Some(OwnerId(2))
        ));
    }

    #[test]
    fn cancelling_fifo_head_immediately_dispatches_unblocked_waiter() {
        let mut ledger = ResourceLedger::new(vector(10, 2, 0), 2, 2, 20);
        let first = ledger.request(OwnerId(1), vector(5, 0, 0));
        let Ok(Admission::Granted(_first)) = first else {
            return assert!(matches!(first, Ok(Admission::Granted(_))));
        };
        let head = ledger.request(OwnerId(2), vector(10, 0, 0));
        let Ok(Admission::Queued(head)) = head else {
            return assert!(matches!(head, Ok(Admission::Queued(_))));
        };
        assert!(matches!(
            ledger.request(OwnerId(3), vector(5, 0, 0)),
            Ok(Admission::Queued(_))
        ));
        let dispatched = ledger.cancel_waiter(head);
        assert!(matches!(
            dispatched,
            Ok(grants) if grants.first().map(|grant| grant.owner) == Some(OwnerId(3))
        ));
        assert_eq!(ledger.waiter_occupancy(), (0, 0));
        assert!(ledger.verify().is_ok());
    }

    #[test]
    fn versioned_contract_builds_exact_declared_owner_ceiling() -> Result<(), String> {
        let contract = ResourceContractV1::new(vector(10, 1, 0), 3, 1, 3, 30);
        let mut ledger = ResourceLedger::from_contract(&contract)
            .map_err(|error| format!("contract rejected: {error:?}"))?;
        assert!(matches!(
            ledger.request(OwnerId(1), vector(1, 0, 0)),
            Ok(Admission::Granted(_))
        ));
        assert!(matches!(
            ledger.request(OwnerId(1), vector(1, 0, 0)),
            Ok(Admission::Queued(_))
        ));
        assert_eq!(ledger.waiter_occupancy(), (1, 1));
        assert!(ledger.verify().is_ok());
        Ok(())
    }

    #[test]
    fn owned_resource_lease_releases_on_normal_error_and_panic_paths() -> Result<(), String> {
        let contract = ResourceContractV1::new(vector(10, 1, 0), 2, 1, 2, 20);
        let broker = OwnedResourceBroker::from_contract(&contract)
            .map_err(|error| format!("broker construction failed: {error:?}"))?;

        {
            let admission = broker.request(OwnerId(1), vector(4, 0, 0));
            assert!(matches!(admission, Ok(OwnedAdmission::Granted(_))));
            assert_eq!(broker.in_use(), vector(4, 0, 0));
            assert_eq!(broker.active_grant_count(), 1);
        }
        assert_eq!(broker.in_use(), ResourceVector::default());
        assert_eq!(broker.active_grant_count(), 0);

        let error_path = || -> Result<(), LedgerError> {
            let admission = broker.request(OwnerId(2), vector(5, 0, 0))?;
            let OwnedAdmission::Granted(_lease) = admission else {
                return Err(LedgerError::InvariantViolation);
            };
            Err(LedgerError::RequestExceedsCapacity)
        };
        assert_eq!(error_path(), Err(LedgerError::RequestExceedsCapacity));
        assert_eq!(broker.in_use(), ResourceVector::default());

        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let admission = broker
                .request(OwnerId(3), vector(6, 0, 0))
                .expect("panic-path grant");
            let OwnedAdmission::Granted(_lease) = admission else {
                panic!("panic-path request unexpectedly queued");
            };
            panic!("injected unwind");
        }));
        assert!(panic_result.is_err());
        assert_eq!(broker.in_use(), ResourceVector::default());
        assert!(broker.verify().is_ok());
        Ok(())
    }

    #[test]
    fn owned_waiter_drop_cancels_exact_head_and_dispatches_next_fifo_owner() -> Result<(), String> {
        let contract = ResourceContractV1::new(vector(10, 0, 0), 2, 1, 3, 30);
        let broker = OwnedResourceBroker::from_contract(&contract)
            .map_err(|error| format!("broker construction failed: {error:?}"))?;
        let active = match broker.request(OwnerId(1), vector(5, 0, 0)) {
            Ok(OwnedAdmission::Granted(lease)) => lease,
            other => return Err(format!("initial request not granted: {other:?}")),
        };
        let blocked_same_owner = match broker.request(OwnerId(1), vector(5, 0, 0)) {
            Ok(OwnedAdmission::Queued(waiter)) => waiter,
            other => return Err(format!("owner ceiling request not queued: {other:?}")),
        };
        let mut next_owner = match broker.request(OwnerId(2), vector(5, 0, 0)) {
            Ok(OwnedAdmission::Queued(waiter)) => waiter,
            other => return Err(format!("FIFO follower not queued: {other:?}")),
        };
        let next_id = next_owner.id();

        drop(blocked_same_owner);
        assert_eq!(broker.ready_waiter_ids(), vec![next_id]);
        let next_lease = next_owner
            .try_acquire()
            .map_err(|error| format!("ready acquisition failed: {error:?}"))?
            .ok_or_else(|| "ready waiter had no grant".to_owned())?;
        assert_eq!(next_lease.owner(), OwnerId(2));
        assert_eq!(broker.waiter_occupancy(), (0, 0));

        drop(active);
        drop(next_lease);
        assert_eq!(broker.in_use(), ResourceVector::default());
        assert!(broker.verify().is_ok());
        Ok(())
    }

    #[test]
    // WP009-REG-CREDIT-OWNED-GATE-001
    fn owned_credit_failure_cancel_drop_and_unwind_paths_release_exactly_once() {
        let contract = ByteCreditContractV1::new(10, 2);
        let broker = OwnedByteCreditBroker::from_contract(&contract).expect("valid contract");
        assert!(matches!(
            broker.claim_coupled(
                OwnerId(1),
                CoupledByteReservation {
                    input_stage: ByteCreditStage::DecryptOrPackInput,
                    input_bytes: u64::MAX,
                    output_stage: ByteCreditStage::DecryptOrPackOutput,
                    output_bytes: 1,
                },
            ),
            Err(CreditError::ArithmeticOverflow)
        ));
        assert_eq!(broker.global_occupancy(), Ok((0, 0)));

        {
            let lease = broker
                .claim_coupled(
                    OwnerId(1),
                    CoupledByteReservation {
                        input_stage: ByteCreditStage::DecryptOrPackInput,
                        input_bytes: 4,
                        output_stage: ByteCreditStage::DecryptOrPackOutput,
                        output_bytes: 6,
                    },
                )
                .expect("coupled exact-capacity claim");
            assert_eq!(
                lease.attribution().expect("attribution").components.len(),
                2
            );
            assert_eq!(broker.global_occupancy(), Ok((2, 10)));
        }
        assert_eq!(broker.global_occupancy(), Ok((0, 0)));

        let cancelled = broker
            .claim(OwnerId(2), ByteCreditStage::Writer, 3)
            .expect("cancel-path claim")
            .cancel()
            .expect("cancel releases");
        assert_eq!(cancelled.components[0].bytes, 3);
        assert_eq!(broker.global_occupancy(), Ok((0, 0)));

        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _lease = broker
                .claim(OwnerId(3), ByteCreditStage::Journal, 5)
                .expect("panic-path claim");
            panic!("injected unwind");
        }));
        assert!(panic_result.is_err());
        assert_eq!(broker.global_occupancy(), Ok((0, 0)));
        assert!(broker.verify().is_ok());
    }
}
