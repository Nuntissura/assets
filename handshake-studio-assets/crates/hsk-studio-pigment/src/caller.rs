//! Example production source caller: exclusive ownership serializes context, sink and lease.
//! This is injectable in-memory source state, never a catalog, queue or authenticated host.
use crate::*;
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_observe::{
    Budget, DeliveryClass, DeliveryError, FailureCode, Observation, Observe, ResourceDetail,
    ResourceDisposition, SinkPort, TileAddress,
};
use std::sync::{Arc, Mutex};
#[derive(Clone, Copy, Debug, Default)]
pub struct Accounting {
    pub live_bytes: u64,
    pub reserved: u64,
    pub released: u64,
    pub transferred: u64,
    pub scratch_released: u64,
    pub provider_live: u64,
    pub provider_peak: u64,
}
#[derive(Debug)]
struct Ledger {
    limit: u64,
    next: u64,
    state: Accounting,
}
#[derive(Clone)]
pub struct MemoryAdmission {
    ledger: Arc<Mutex<Ledger>>,
    operation_limit: u64,
}
impl MemoryAdmission {
    pub fn new(limit: u64) -> Result<Self, Error> {
        if limit == 0 || limit > MAX_BYTES {
            return Err(Error::BudgetExceeded);
        }
        Ok(Self {
            operation_limit: limit,
            ledger: Arc::new(Mutex::new(Ledger {
                limit,
                next: 1,
                state: Accounting::default(),
            })),
        })
    }
    pub fn scoped(&self, limit: u64) -> Result<Self, Error> {
        if limit == 0 || limit > MAX_BYTES {
            return Err(Error::BudgetExceeded);
        }
        Ok(Self {
            ledger: self.ledger.clone(),
            operation_limit: self.operation_limit.min(limit),
        })
    }
    pub fn accounting(&self) -> Accounting {
        self.ledger.lock().unwrap_or_else(|p| p.into_inner()).state
    }
    pub fn reserve(&mut self, counts: Counts, limit: u64) -> Result<Lease, Error> {
        let bytes = counts.bytes()?;
        let mut ledger = self.ledger.lock().unwrap_or_else(|p| p.into_inner());
        let total = ledger
            .state
            .live_bytes
            .checked_add(bytes)
            .ok_or(Error::Overflow)?;
        if bytes > limit || total > ledger.limit.min(self.operation_limit).min(limit) {
            return Err(Error::LeaseUnavailable);
        }
        let handle = ledger.next;
        let next = handle.checked_add(1).ok_or(Error::Overflow)?;
        let reserved = ledger.state.reserved.saturating_add(bytes);
        // Every fallible admission check precedes any ledger mutation.
        ledger.next = next;
        ledger.state.live_bytes = total;
        ledger.state.reserved = reserved;
        drop(ledger);
        Ok(Lease::new(Box::new(MemoryLease {
            ledger: self.ledger.clone(),
            handle,
            counts,
            active: true,
            transferred: false,
        })))
    }
}
/// Inline token shares the caller's already owned ledger; no new container allocation.
/// Fields holding provider data are dropped by Prism before this token retires its charge.
#[derive(Debug)]
pub struct ProviderLease {
    ledger: Arc<Mutex<Ledger>>,
    bytes: u64,
}
impl hsk_studio_prism::ProviderReservation for ProviderLease {
    fn reserved_bytes(&self) -> u64 {
        self.bytes
    }
}
impl Drop for ProviderLease {
    fn drop(&mut self) {
        let mut ledger = self.ledger.lock().unwrap_or_else(|p| p.into_inner());
        ledger.state.live_bytes -= self.bytes;
        ledger.state.provider_live -= self.bytes;
        ledger.state.released = ledger.state.released.saturating_add(self.bytes);
    }
}
impl hsk_studio_prism::ProviderAdmission for MemoryAdmission {
    type Reservation = ProviderLease;
    fn synchronous_retirement(&self) -> bool {
        true
    }
    fn reserve(&self, bytes: u64) -> Result<ProviderLease, hsk_studio_prism::Error> {
        let mut ledger = self.ledger.lock().unwrap_or_else(|p| p.into_inner());
        let total = ledger
            .state
            .live_bytes
            .checked_add(bytes)
            .ok_or(hsk_studio_prism::Error::AdmissionDenied)?;
        if total > ledger.limit.min(self.operation_limit) {
            return Err(hsk_studio_prism::Error::AdmissionDenied);
        }
        let provider_live = ledger
            .state
            .provider_live
            .checked_add(bytes)
            .ok_or(hsk_studio_prism::Error::AdmissionDenied)?;
        ledger.state.live_bytes = total;
        ledger.state.provider_live = provider_live;
        ledger.state.provider_peak = ledger.state.provider_peak.max(provider_live);
        ledger.state.reserved = ledger.state.reserved.saturating_add(bytes);
        drop(ledger);
        Ok(ProviderLease {
            ledger: self.ledger.clone(),
            bytes,
        })
    }
}
struct MemoryLease {
    ledger: Arc<Mutex<Ledger>>,
    handle: u64,
    counts: Counts,
    active: bool,
    transferred: bool,
}
impl LeasePort for MemoryLease {
    fn handle(&self) -> u64 {
        self.handle
    }
    fn counts(&self) -> Counts {
        self.counts
    }
    fn reduce(&mut self, counts: Counts) -> Result<(), Error> {
        let old = self.counts.bytes()?;
        let new = counts.bytes()?;
        if new > old {
            return Err(Error::BudgetExceeded);
        }
        let mut ledger = self.ledger.lock().unwrap_or_else(|p| p.into_inner());
        ledger.state.live_bytes -= old - new;
        ledger.state.released = ledger.state.released.saturating_add(old - new);
        ledger.state.scratch_released = ledger.state.scratch_released.saturating_add(
            self.counts
                .scratch_bytes
                .saturating_sub(counts.scratch_bytes),
        );
        self.counts = counts;
        Ok(())
    }
    fn mark_transferred(&mut self) {
        if !self.transferred {
            self.transferred = true;
            let mut l = self.ledger.lock().unwrap_or_else(|p| p.into_inner());
            l.state.transferred = l
                .state
                .transferred
                .saturating_add(self.counts.bytes().expect("admitted counts"))
        }
    }
    fn release(&mut self) {
        if self.active {
            self.active = false;
            let mut l = self.ledger.lock().unwrap_or_else(|p| p.into_inner());
            let bytes = self.counts.bytes().expect("admitted counts");
            l.state.live_bytes -= bytes;
            l.state.released = l.state.released.saturating_add(bytes)
        }
    }
}
#[derive(Default)]
pub struct FrameCollector {
    pub frames: Vec<Vec<u8>>,
}
impl SinkPort for FrameCollector {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        if class != DeliveryClass::Terminal
            || bytes.len() > hsk_studio_observe::MAX_FRAME_BYTES
            || self.frames.len() >= 65
        {
            return Err(DeliveryError::Saturated);
        }
        self.frames.push(bytes.to_vec());
        Ok(())
    }
}
/// One admitted frame capacity; no growing container during provider callbacks.
pub(crate) struct ConversionFrame {
    frame: Vec<u8>,
    sent: bool,
}
impl ConversionFrame {
    pub fn new() -> Self {
        Self {
            frame: Vec::with_capacity(hsk_studio_observe::MAX_FRAME_BYTES),
            sent: false,
        }
    }
    pub fn into_frame(self) -> Vec<u8> {
        self.frame
    }
}
impl SinkPort for ConversionFrame {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        if self.sent || class != DeliveryClass::Terminal || bytes.len() > self.frame.capacity() {
            return Err(DeliveryError::Saturated);
        }
        self.frame.extend_from_slice(bytes);
        self.sent = true;
        Ok(())
    }
}
struct Pending {
    prepared: Prepared,
    document: DomainId,
    layer: DomainId,
    command: String,
    correlation: u64,
    actor: ActorContext,
    epoch: u64,
    base_revision: u64,
    metadata_lease: ProviderLease,
}
pub struct SerializedOwner<S: SinkPort> {
    document: DomainId,
    layer: DomainId,
    actor: ActorContext,
    epoch: u64,
    revisions: Vec<(TileAddress, u64)>,
    pub admission: MemoryAdmission,
    pub sink: S,
    pending: [Option<(String, Pending)>; MAX_TILES],
    next_pending: u64,
}
impl<S: SinkPort> SerializedOwner<S> {
    pub fn new(
        document: DomainId,
        layer: DomainId,
        actor: ActorContext,
        epoch: u64,
        revisions: Vec<(TileAddress, u64)>,
        admission: MemoryAdmission,
        sink: S,
    ) -> Result<Self, Error> {
        if document.prefix() != "SDOC"
            || layer.prefix() != "SLYR"
            || revisions.len() > MAX_TILES
            || revisions
                .iter()
                .enumerate()
                .any(|(i, (a, _))| revisions[..i].iter().any(|(b, _)| a == b))
        {
            return Err(Error::InvalidInput);
        }
        Ok(Self {
            document,
            layer,
            actor,
            epoch,
            revisions,
            admission,
            sink,
            pending: std::array::from_fn(|_| None),
            next_pending: 1,
        })
    }
    pub fn set_epoch(&mut self, epoch: u64) {
        self.epoch = epoch
    }
    pub fn set_revision(&mut self, address: TileAddress, revision: u64) -> Result<(), Error> {
        let value = self
            .revisions
            .iter_mut()
            .find(|(a, _)| *a == address)
            .ok_or(Error::RevisionUnavailable)?;
        value.1 = revision;
        Ok(())
    }
    pub fn pending_count(&self) -> usize {
        self.pending.iter().filter(|p| p.is_some()).count()
    }
    /// Explicit caller reconciliation, not automatic retry. Only a known rejection may free pending bytes.
    pub fn reconcile_rejected(&mut self, token: &str) -> Result<(), Error> {
        let i = self
            .pending
            .iter()
            .position(|p| p.as_ref().is_some_and(|(t, _)| t == token))
            .ok_or(Error::InvalidInput)?;
        drop(self.pending[i].take());
        Ok(())
    }
    /// Caller supplies independently resolved delivery receipt. Fresh source preconditions remain mandatory.
    pub fn reconcile_accepted(
        &mut self,
        token: &str,
        receipt: hsk_studio_observe::Receipt,
        cancel: &CancellationToken,
    ) -> Result<Patch, Error> {
        let i = self
            .pending
            .iter()
            .position(|p| p.as_ref().is_some_and(|(t, _)| t == token))
            .ok_or(Error::InvalidInput)?;
        let p = &self.pending[i].as_ref().expect("located pending").1;
        if cancel.check().is_err()
            || self.epoch != p.epoch
            || self.document != p.document
            || self.layer != p.layer
            || self.actor != p.actor
        {
            return Err(Error::StaleEpoch);
        }
        if receipt.correlation_id != p.correlation
            || receipt.revision != p.base_revision
            || receipt.outcome != hsk_studio_observe::Outcome::Success
        {
            return Err(Error::ReconciliationRequired);
        }
        for u in &p.prepared.updates {
            if self
                .revisions
                .iter()
                .find(|(a, _)| a == &u.address)
                .map(|(_, v)| *v)
                != Some(u.previous_revision)
            {
                return Err(Error::RevisionConflict);
            }
        }
        let (_, p) = self.pending[i].take().expect("located pending");
        Ok(self.accept(
            p.prepared,
            p.document,
            p.command,
            p.correlation,
            p.actor,
            p.epoch,
            receipt,
            p.metadata_lease,
        ))
    }
    #[allow(clippy::too_many_arguments)]
    fn accept(
        &mut self,
        mut prepared: Prepared,
        document: DomainId,
        command: String,
        correlation: u64,
        actor: ActorContext,
        epoch: u64,
        receipt: hsk_studio_observe::Receipt,
        metadata_lease: ProviderLease,
    ) -> Patch {
        for update in &prepared.updates {
            let revision = self
                .revisions
                .iter_mut()
                .find(|(a, _)| a == &update.address)
                .expect("final checked revision");
            revision.1 = update.revision
        }
        if let Some(lease) = &mut prepared.lease {
            lease.mark_transferred()
        }
        Patch {
            document_id: document,
            command_id: command,
            correlation_id: correlation,
            actor,
            cancel_epoch: epoch,
            updates: prepared.updates,
            counts: prepared.counts,
            lease: prepared.lease,
            diagnostic: receipt,
            metadata_lease,
        }
    }
    fn deliver(
        &mut self,
        r: &Request<'_>,
        mut counts: Counts,
        disposition: ResourceDisposition,
        error: Option<Error>,
        cancel: &CancellationToken,
    ) -> Result<hsk_studio_observe::Receipt, hsk_studio_observe::Error> {
        let diagnostic_bytes = (r.document_id.as_str().len()
            + crate::masked_patch::actor_bytes(r.actor)
            + r.layer_id.as_str().len()
            + 128
            + hsk_studio_observe::MAX_FRAME_BYTES) as u64;
        let _diagnostic_lease = if disposition == ResourceDisposition::Rejected {
            let admission = self
                .admission
                .scoped(r.byte_limit)
                .map_err(|_| hsk_studio_observe::Error::InvalidBudget)?;
            let lease = hsk_studio_prism::ProviderAdmission::reserve(&admission, diagnostic_bytes)
                .map_err(|_| hsk_studio_observe::Error::InvalidBudget)?;
            if counts.bytes().unwrap_or(0) < diagnostic_bytes {
                counts.output_bytes = diagnostic_bytes;
                counts.scratch_bytes = 0;
                counts.retained_preimage_bytes = 0;
            }
            Some(lease)
        } else {
            None
        }; // Finalization's retained metadata reservation covers this frame/context.
        let detail = ResourceDetail::new(
            disposition,
            error.map(Error::diagnostic),
            r.tiles.first().and_then(|t| address(t).ok()),
            r.cancel_epoch,
            counts
                .diagnostic(r.byte_limit)
                .map_err(|_| hsk_studio_observe::Error::InvalidDetail)?,
        )?;
        let mut observe = Observe::new(
            r.correlation_id,
            r.base_revision,
            r.document_id.clone(),
            r.actor.clone(),
            Budget::new(0, 0)?,
        );
        let outcome = match error {
            None => hsk_studio_observe::Outcome::Success,
            Some(Error::Canceled) => hsk_studio_observe::Outcome::Canceled,
            Some(_) => hsk_studio_observe::Outcome::Failure(FailureCode::Validation),
        };
        observe.emit_resource(
            Observation {
                correlation_id: r.correlation_id,
                revision: r.base_revision,
                outcome,
                progress: None,
                private_project_text: None,
            },
            &detail,
            cancel,
            &mut self.sink,
        )
    }
}
impl<S: SinkPort> ExecutionPort for SerializedOwner<S> {
    fn provider_peak_bytes(&mut self) -> u64 {
        self.admission.accounting().provider_peak
    }
    fn current_epoch(&mut self) -> Result<u64, Error> {
        Ok(self.epoch)
    }
    fn current_membership(&mut self, document: &DomainId, layer: &DomainId) -> Result<(), Error> {
        if document != &self.document || layer != &self.layer {
            return Err(Error::MembershipUnavailable);
        }
        Ok(())
    }
    fn current_revision(
        &mut self,
        document: &DomainId,
        layer: &DomainId,
        address: &TileAddress,
    ) -> Result<u64, Error> {
        if document != &self.document || layer != &self.layer {
            return Err(Error::MembershipUnavailable);
        }
        self.revisions
            .iter()
            .find(|(a, _)| a == address)
            .map(|(_, v)| *v)
            .ok_or(Error::RevisionUnavailable)
    }
    fn provider_admission(&mut self, byte_limit: u64) -> Result<MemoryAdmission, Error> {
        self.admission.scoped(byte_limit)
    }
    fn lease_layout_bytes(&mut self) -> Result<u64, Error> {
        crate::Lease::allocation_bytes()?
            .checked_add(std::mem::size_of::<MemoryLease>() as u64)
            .ok_or(Error::Overflow)
    }
    fn reserve(&mut self, counts: Counts, limit: u64) -> Result<Lease, Error> {
        self.admission.reserve(counts, limit)
    }
    fn reject(
        &mut self,
        r: &Request<'_>,
        error: Error,
        mut counts: Counts,
        cancel: &CancellationToken,
    ) -> Outcome {
        counts.changed = 0; // invalid/unadmitted claims are not counted reservations.
        if counts.admitted == 0
            || counts.bytes().is_err()
            || counts.bytes().is_ok_and(|b| b > r.byte_limit)
        {
            counts.output_bytes = 0;
            counts.scratch_bytes = 0;
            counts.retained_preimage_bytes = 0
        }
        if counts.attempted > MAX_TILES as u32 {
            counts.attempted = MAX_TILES as u32;
            counts.admitted = 0
        }
        let delivery = self.deliver(
            r,
            counts,
            ResourceDisposition::Rejected,
            Some(error),
            cancel,
        );
        Outcome::Rejected(Failure {
            error,
            counts,
            delivery,
        })
    }
    fn finalize(
        &mut self,
        r: &Request<'_>,
        mut prepared: Prepared,
        cancel: &CancellationToken,
    ) -> Outcome {
        let metadata_bytes = (r.document_id.as_str().len() * 2
            + r.layer_id.as_str().len() * 2
            + r.command_id.len()
            + crate::masked_patch::actor_bytes(r.actor) * 2
            + 256
            + hsk_studio_observe::MAX_FRAME_BYTES) as u64
            + std::alloc::Layout::new::<Patch>().size() as u64;
        let admission = match self.admission.scoped(r.byte_limit) {
            Ok(admission) => admission,
            Err(error) => return self.reject(r, error, prepared.counts, cancel),
        };
        let metadata_lease =
            match hsk_studio_prism::ProviderAdmission::reserve(&admission, metadata_bytes) {
                Ok(lease) => lease,
                Err(_) => return self.reject(r, Error::LeaseUnavailable, prepared.counts, cancel),
            };
        // Report one simultaneous reservation-peak tuple, never sum maxima from
        // retired provider scratch and a later finalization metadata phase.
        let final_output = if prepared.lease.is_some() {
            prepared.counts.output_bytes.checked_add(metadata_bytes)
        } else {
            Some(metadata_bytes)
        };
        let final_preimage = if prepared.lease.is_some() {
            prepared.counts.retained_preimage_bytes
        } else {
            0
        };
        let final_total = match final_output.and_then(|v| v.checked_add(final_preimage)) {
            Some(total) if total <= r.byte_limit => total,
            _ => return self.reject(r, Error::BudgetExceeded, prepared.counts, cancel),
        };
        if final_total > prepared.counts.bytes().unwrap_or(0) {
            prepared.counts.output_bytes = final_output.expect("checked final tuple");
            prepared.counts.scratch_bytes = 0;
            prepared.counts.retained_preimage_bytes = final_preimage;
        }
        if self.actor != *r.actor {
            return self.reject(r, Error::MembershipUnavailable, prepared.counts, cancel);
        }
        if let Err(e) = fresh(self, r, cancel) {
            return self.reject(r, e, prepared.counts, cancel);
        }
        if self.pending.iter().all(Option::is_some) || self.next_pending == u64::MAX {
            return self.reject(r, Error::BudgetExceeded, prepared.counts, cancel);
        }
        let disposition = if prepared.updates.is_empty() {
            ResourceDisposition::NoChange
        } else {
            ResourceDisposition::Changed
        };
        // Required conversion diagnostics are held as actual bytes, then delivered within
        // this same serialized fresh-context/ownership boundary before terminal acceptance.
        let mut conversion_failure = None;
        for update in &prepared.updates {
            for frame in &update.conversion_frames {
                if let Err(error) = self.sink.try_send(DeliveryClass::Terminal, frame) {
                    conversion_failure = Some(hsk_studio_observe::Error::Delivery(error));
                    break;
                }
            }
            if conversion_failure.is_some() {
                break;
            }
        }
        let delivery = match conversion_failure {
            Some(error) => Err(error),
            None => self.deliver(r, prepared.counts, disposition, None, cancel),
        };
        match delivery {
            Ok(receipt)
                if receipt.outcome == hsk_studio_observe::Outcome::Success
                    && fresh(self, r, cancel).is_ok() =>
            {
                Outcome::Accepted(Box::new(self.accept(
                    prepared,
                    r.document_id.clone(),
                    r.command_id.into(),
                    r.correlation_id,
                    r.actor.clone(),
                    r.cancel_epoch,
                    receipt,
                    metadata_lease,
                )))
            }
            Ok(_)
            | Err(hsk_studio_observe::Error::Delivery(DeliveryError::Indeterminate))
            | Err(hsk_studio_observe::Error::ReconciliationRequired) => {
                // Callback-time context/cancellation change or indeterminate sink retains ownership.
                let mut token = String::with_capacity(64);
                use std::fmt::Write as _;
                write!(&mut token, "pigment-pending-{}", self.next_pending).expect("bounded token");
                self.next_pending += 1;
                let counts = prepared.counts;
                let slot = self
                    .pending
                    .iter_mut()
                    .find(|p| p.is_none())
                    .expect("bounded pending slot");
                *slot = Some((
                    token.clone(),
                    Pending {
                        prepared,
                        document: r.document_id.clone(),
                        layer: r.layer_id.clone(),
                        command: r.command_id.into(),
                        correlation: r.correlation_id,
                        actor: r.actor.clone(),
                        epoch: r.cancel_epoch,
                        base_revision: r.base_revision,
                        metadata_lease,
                    },
                ));
                Outcome::ReconciliationRequired {
                    token,
                    counts,
                    delivery: hsk_studio_observe::Error::ReconciliationRequired,
                }
            }
            Err(error) => Outcome::Rejected(Failure {
                error: Error::DeliveryFailed,
                counts: Counts {
                    changed: 0,
                    ..prepared.counts
                },
                delivery: Err(error),
            }),
        }
    }
}
