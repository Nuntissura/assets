//! The host side of the bridge: a byte-level `HostBridge` and a transient, process-local host
//! that really enforces what a canonical authority must (per-principal grants rechecked at
//! commit, lease-epoch fence, expected-revision compare, idempotency bound to principal,
//! resource and binding, monotonic receipts). It is memory only: nothing it holds survives the
//! process, and it is a stand-in for the concrete host adapters that live in the product.
//!
//! Proven by `tests/bridge_contract.rs`: grants and commit order (`fail_closed_on_absent_or_revoked_grant`,
//! `roundtrip_envelopes_through_session_and_idempotent_replay`), job tickets bound to their owner
//! (`job_tickets_are_released_only_by_their_owner`, `job_admission_needs_a_standing_grant`), and a host-owned lease epoch
//! (`lease_epoch_is_issued_by_the_host_not_adopted_from_callers`).
#![deny(clippy::arithmetic_side_effects)]
use crate::business_event::StudioBusinessEvent;
use crate::wire::{
    ActionWire, Body, CommitDecision, CommitMsg, DenyReason, DiagnosticMsg, Envelope, ErrorReply,
    GrantDecision, JobDecision, JobDenyCode, Limits, LookupDecision, ReceiptWire, RejectCode,
    Reply, ReplyBody, decode_envelope, encode_reply, hex_decode,
};
use hsk_studio_accord::{
    ActorContext, BusinessEventDeliveryError, BusinessEventRecord, BusinessEventSink, DomainId,
};
use hsk_studio_session::Action;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Mutex, MutexGuard, PoisonError},
};

// V4-04: every fault-injection hook lives in `host/faults.rs`, compiled only under `cfg(test)` or
// the non-default `test-faults` feature. Test `fault_hooks_exist_only_under_the_test_faults_feature`
// scans for it; the compile_fail doctests in `lib.rs` show a default build has no such item.
#[cfg(any(test, feature = "test-faults"))]
mod faults;
#[cfg(any(test, feature = "test-faults"))]
pub use faults::Fault;

/// A host reachable as bytes in, bytes out. A real adapter (shared client, broker, ledger) is a
/// drop-in; an empty or undecodable return is "no usable reply" to the caller.
pub trait HostBridge: Send + Sync {
    fn handle(&self, request: &[u8]) -> Vec<u8>;
}

impl<T: HostBridge + ?Sized> HostBridge for std::sync::Arc<T> {
    fn handle(&self, request: &[u8]) -> Vec<u8> {
        (**self).handle(request)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HostConfig {
    pub limits: Limits,
    pub job_capacity: u32,
    pub job_byte_budget: u64,
    pub retry_after_ms: u64,
    /// Idempotency entries (and receipts); a new key past this is refused, nothing is evicted.
    pub max_receipts: usize,
    pub max_diagnostics: usize,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            job_capacity: 4,
            job_byte_budget: 64 * 1024 * 1024,
            retry_after_ms: 50,
            max_receipts: 1024,
            max_diagnostics: 1024,
        }
    }
}

/// A diagnostic the host recorded, for independent inspection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticRecord {
    pub correlation_id: u64,
    pub message: DiagnosticMsg,
}

type GrantKey = (String, String, String, ActionWire);

struct ResourceState {
    revision: u64,
    epoch: u64,
    document: Vec<u8>,
}

struct IdempotencyEntry {
    account: String,
    principal: String,
    resource: String,
    binding: String,
    receipt: ReceiptWire,
}

/// An admitted job. The ticket belongs to the account, principal and resource it was admitted
/// for; only that owner can release it.
struct JobEntry {
    account: String,
    principal: String,
    resource: String,
    jobs: u32,
    bytes: u64,
}

#[derive(Default)]
struct State {
    grants: BTreeSet<GrantKey>,
    /// Grants that stood and were withdrawn (answered as `revoked`, not `missing`).
    revoked: BTreeSet<GrantKey>,
    /// Token issued for a currently standing grant (revocation removes it).
    tokens: BTreeMap<GrantKey, u64>,
    next_token: u64,
    resources: BTreeMap<String, ResourceState>,
    idempotency: BTreeMap<String, IdempotencyEntry>,
    receipts: Vec<ReceiptWire>,
    sequence: u64,
    jobs: BTreeMap<u64, JobEntry>,
    next_ticket: u64,
    diagnostics: Vec<DiagnosticRecord>,
    #[cfg(any(test, feature = "test-faults"))]
    faults: std::collections::VecDeque<faults::Fault>,
    requests: u64,
}

pub struct TransientHost {
    config: HostConfig,
    state: Mutex<State>,
}

fn grant_key(actor: &ActorContext, resource: &DomainId, action: Action) -> GrantKey {
    (
        actor.account_id().to_owned(),
        actor.principal_id().to_owned(),
        resource.as_str().to_owned(),
        action.into(),
    )
}

impl Default for TransientHost {
    fn default() -> Self {
        Self::new(HostConfig::default())
    }
}

impl TransientHost {
    pub fn new(config: HostConfig) -> Self {
        Self {
            config,
            state: Mutex::new(State {
                next_token: 1,
                next_ticket: 1,
                ..State::default()
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // ----- control (the "host operator" side; not reachable over the wire) ---------------------

    pub fn grant(&self, actor: &ActorContext, resource: &DomainId, action: Action) {
        let key = grant_key(actor, resource, action);
        let mut state = self.lock();
        state.revoked.remove(&key);
        state.grants.insert(key);
    }

    pub fn revoke(&self, actor: &ActorContext, resource: &DomainId, action: Action) {
        let key = grant_key(actor, resource, action);
        let mut state = self.lock();
        if state.grants.remove(&key) {
            state.revoked.insert(key.clone());
        }
        state.tokens.remove(&key);
    }

    /// Creates (or resets) the authoritative copy of a resource.
    pub fn seed_resource(&self, resource: &DomainId, revision: u64, epoch: u64, document: &[u8]) {
        self.lock().resources.insert(
            resource.as_str().to_owned(),
            ResourceState {
                revision,
                epoch,
                document: document.to_vec(),
            },
        );
    }

    // ----- independent inspection (the oracle for tests) ---------------------------------------

    pub fn committed_revision(&self, resource: &DomainId) -> Option<u64> {
        self.lock().resources.get(resource.as_str()).map(|r| r.revision)
    }

    pub fn lease_epoch(&self, resource: &DomainId) -> Option<u64> {
        self.lock().resources.get(resource.as_str()).map(|r| r.epoch)
    }

    pub fn document_bytes(&self, resource: &DomainId) -> Option<Vec<u8>> {
        self.lock()
            .resources
            .get(resource.as_str())
            .map(|r| r.document.clone())
    }

    pub fn receipts(&self) -> Vec<ReceiptWire> {
        self.lock().receipts.clone()
    }

    pub fn idempotency_len(&self) -> usize {
        self.lock().idempotency.len()
    }

    pub fn diagnostics(&self) -> Vec<DiagnosticRecord> {
        self.lock().diagnostics.clone()
    }

    /// (jobs, bytes) admitted and not yet released.
    pub fn jobs_in_flight(&self) -> (u32, u64) {
        jobs_used(&self.lock())
    }

    pub fn requests_seen(&self) -> u64 {
        self.lock().requests
    }

    // ----- protocol -----------------------------------------------------------------------------

    fn dispatch(&self, envelope: Envelope) -> (Reply, bool) {
        let correlation = envelope.correlation_id;
        let mut state = self.lock();
        let mut drop_ack = false;
        let result = match &envelope.body {
            Body::GrantCheck { action } => self.grant_check(&mut state, &envelope, *action),
            Body::Commit(commit) => {
                let (decision, dropped) = self.commit(&mut state, &envelope, commit);
                drop_ack = dropped;
                ReplyBody::Commit(decision)
            }
            Body::Lookup => lookup(&state, &envelope),
            Body::JobAdmit {
                action,
                jobs,
                bytes,
            } => self.job_admit(&mut state, &envelope, *action, *jobs, *bytes),
            Body::JobRelease { ticket } => job_release(&mut state, &envelope, *ticket),
            Body::Diagnostic(message) => {
                if state.diagnostics.len() < self.config.max_diagnostics {
                    state.diagnostics.push(DiagnosticRecord {
                        correlation_id: correlation,
                        message: message.clone(),
                    });
                }
                ReplyBody::Ack
            }
        };
        (Reply::new(correlation, result), drop_ack)
    }

    fn grant_check(&self, state: &mut State, envelope: &Envelope, action: ActionWire) -> ReplyBody {
        let key = (
            envelope.actor.account_id.clone(),
            envelope.actor.principal_id.clone(),
            envelope.resource_id.clone(),
            action,
        );
        if !state.grants.contains(&key) {
            let reason = if state.revoked.contains(&key) {
                DenyReason::Revoked
            } else {
                DenyReason::Missing
            };
            return ReplyBody::Grant(GrantDecision {
                granted: false,
                token: 0,
                reason: Some(reason),
            });
        }
        let token = match state.tokens.get(&key) {
            Some(token) => *token,
            None => {
                let token = state.next_token;
                // An exhausted counter issues nothing (fail closed) instead of wrapping.
                let Some(next) = token.checked_add(1) else {
                    return ReplyBody::Grant(GrantDecision {
                        granted: false,
                        token: 0,
                        reason: Some(DenyReason::Missing),
                    });
                };
                state.next_token = next;
                state.tokens.insert(key, token);
                token
            }
        };
        ReplyBody::Grant(GrantDecision {
            granted: true,
            token,
            reason: None,
        })
    }

    /// Admission is authorized like any other request: the principal must hold a standing grant
    /// for the resource and action, and the ticket it receives is bound to that account,
    /// principal and resource.
    fn job_admit(
        &self,
        state: &mut State,
        envelope: &Envelope,
        action: ActionWire,
        jobs: u32,
        bytes: u64,
    ) -> ReplyBody {
        let key = (
            envelope.actor.account_id.clone(),
            envelope.actor.principal_id.clone(),
            envelope.resource_id.clone(),
            action,
        );
        if !state.grants.contains(&key) {
            let code = if state.revoked.contains(&key) {
                JobDenyCode::GrantRevoked
            } else {
                JobDenyCode::GrantMissing
            };
            return ReplyBody::Job(JobDecision::Denied { code });
        }
        let (used_jobs, used_bytes) = jobs_used(state);
        let fits = used_jobs.checked_add(jobs).is_some_and(|n| n <= self.config.job_capacity)
            && used_bytes
                .checked_add(bytes)
                .is_some_and(|n| n <= self.config.job_byte_budget);
        if !fits {
            return ReplyBody::Job(JobDecision::Refused {
                retry_after_ms: self.config.retry_after_ms,
            });
        }
        let ticket = state.next_ticket;
        let Some(next) = ticket.checked_add(1) else {
            return ReplyBody::Job(JobDecision::Refused {
                retry_after_ms: self.config.retry_after_ms,
            });
        };
        state.next_ticket = next;
        state.jobs.insert(
            ticket,
            JobEntry {
                account: envelope.actor.account_id.clone(),
                principal: envelope.actor.principal_id.clone(),
                resource: envelope.resource_id.clone(),
                jobs,
                bytes,
            },
        );
        ReplyBody::Job(JobDecision::Admitted { ticket })
    }

    /// Returns the decision and whether the acknowledgement is to be lost.
    fn commit(
        &self,
        state: &mut State,
        envelope: &Envelope,
        commit: &CommitMsg,
    ) -> (CommitDecision, bool) {
        let actor = &envelope.actor;
        let resource = envelope.resource_id.as_str();
        let edit_key: GrantKey = (
            actor.account_id.clone(),
            actor.principal_id.clone(),
            resource.to_owned(),
            ActionWire::Edit,
        );
        // Whether this commit's reply is withheld: set only by a planned fault (test-faults).
        #[cfg(any(test, feature = "test-faults"))]
        let lose_reply = faults::before_commit(state, &edit_key, resource);
        #[cfg(not(any(test, feature = "test-faults")))]
        let lose_reply = false;
        let reject = |code| (CommitDecision::Rejected { code }, false);

        // 1. Authorization is rechecked on every commit, replays included, before anything else.
        if !state.grants.contains(&edit_key) {
            return reject(RejectCode::GrantRevoked);
        }
        if state.tokens.get(&edit_key) != Some(&commit.grant_token) {
            return reject(RejectCode::GrantMissing);
        }
        let Some(key) = envelope.idempotency_key.as_deref() else {
            return reject(RejectCode::InvalidRequest);
        };
        // 2. Idempotency: same key and bound tuple replays the stored receipt; anything else on
        //    a known key conflicts and discloses nothing.
        if let Some(entry) = state.idempotency.get(key) {
            let same = entry.account == actor.account_id
                && entry.principal == actor.principal_id
                && entry.resource == resource
                && entry.binding == commit.binding;
            return if same {
                (CommitDecision::Accepted(entry.receipt.clone()), lose_reply)
            } else {
                reject(RejectCode::IdempotencyConflict)
            };
        }
        let Some(current) = state.resources.get(resource) else {
            return reject(RejectCode::ResourceUnknown);
        };
        // 3. Lease-epoch fence, then expected-revision compare. The epoch is host-owned: only the
        //    host's own current epoch passes. An older one is stale; a newer one was not issued by
        //    the host and is not adopted (a caller could otherwise fence every other writer; test
        //    `lease_epoch_is_issued_by_the_host_not_adopted_from_callers`).
        if envelope.lease_epoch < current.epoch {
            return (CommitDecision::StaleEpoch, false);
        }
        if envelope.lease_epoch > current.epoch {
            return reject(RejectCode::EpochNotIssued);
        }
        if envelope.expected_revision != current.revision {
            return (
                CommitDecision::Stale {
                    current_revision: current.revision,
                },
                false,
            );
        }
        if envelope.expected_revision.checked_add(1) != Some(commit.successor_revision) {
            return reject(RejectCode::SuccessorMismatch);
        }
        if state.idempotency.len() >= self.config.max_receipts {
            return reject(RejectCode::ReceiptsFull);
        }
        let Ok(document) = hex_decode(&commit.document_hex, self.config.limits.max_document_bytes)
        else {
            return reject(RejectCode::InvalidRequest);
        };
        // 4. Commit atomically (one lock): state, receipt and idempotency entry together.
        let Some(sequence) = state.sequence.checked_add(1) else {
            return reject(RejectCode::ReceiptsFull);
        };
        state.sequence = sequence;
        let receipt = ReceiptWire {
            revision: commit.successor_revision,
            sequence: state.sequence,
            binding: commit.binding.clone(),
            host_ref: format!("transient-{}", state.sequence),
        };
        if let Some(r) = state.resources.get_mut(resource) {
            r.revision = commit.successor_revision;
            r.document = document;
        }
        state.receipts.push(receipt.clone());
        state.idempotency.insert(
            key.to_owned(),
            IdempotencyEntry {
                account: actor.account_id.clone(),
                principal: actor.principal_id.clone(),
                resource: resource.to_owned(),
                binding: commit.binding.clone(),
                receipt: receipt.clone(),
            },
        );
        (CommitDecision::Accepted(receipt), lose_reply)
    }
}

/// (jobs, bytes) held by admitted, unreleased tickets.
fn jobs_used(state: &State) -> (u32, u64) {
    state.jobs.values().fold((0, 0), |(j, b), entry| {
        (j.saturating_add(entry.jobs), b.saturating_add(entry.bytes))
    })
}

/// Only the owner (account, principal and resource the ticket was admitted for) releases a
/// ticket; any other caller is refused and nothing changes. An unknown ticket (already released)
/// is acknowledged, so a retried release is harmless.
fn job_release(state: &mut State, envelope: &Envelope, ticket: u64) -> ReplyBody {
    let Some(entry) = state.jobs.get(&ticket) else {
        return ReplyBody::Ack;
    };
    let owner = entry.account == envelope.actor.account_id
        && entry.principal == envelope.actor.principal_id
        && entry.resource == envelope.resource_id;
    if !owner {
        return ReplyBody::Job(JobDecision::Denied {
            code: JobDenyCode::NotTicketOwner,
        });
    }
    state.jobs.remove(&ticket);
    ReplyBody::Ack
}

fn lookup(state: &State, envelope: &Envelope) -> ReplyBody {
    let found = envelope
        .idempotency_key
        .as_deref()
        .and_then(|key| state.idempotency.get(key))
        .filter(|entry| {
            entry.account == envelope.actor.account_id
                && entry.principal == envelope.actor.principal_id
                && entry.resource == envelope.resource_id
        });
    match found {
        Some(entry) => ReplyBody::Lookup(LookupDecision::Found(entry.receipt.clone())),
        None => ReplyBody::Lookup(LookupDecision::Absent),
    }
}

impl HostBridge for TransientHost {
    fn handle(&self, request: &[u8]) -> Vec<u8> {
        {
            let mut state = self.lock();
            state.requests = state.requests.saturating_add(1);
        }
        let limits = self.config.limits;
        let (reply, drop_ack) = match decode_envelope(request, &limits) {
            Ok(envelope) => self.dispatch(envelope),
            Err(error) => (
                Reply::new(
                    0,
                    ReplyBody::Error(ErrorReply {
                        code: error.code().to_owned(),
                    }),
                ),
                false,
            ),
        };
        if drop_ack {
            return Vec::new();
        }
        encode_reply(&reply, &limits).unwrap_or_default()
    }
}

// ----- business events (lane SMB-BUSINESS-EVENTS-1) ----------------------------------------------

/// What [`TransientBusinessEvents`] has seen, for independent inspection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransientEventStats {
    /// Events stored.
    pub accepted: u64,
    /// Re-deliveries of an already stored, identical envelope (stored once).
    pub replays: u64,
    /// Refused because the sink was full.
    pub saturated: u64,
    /// Refused because the event id was stored with a different envelope.
    pub collisions: u64,
    /// Records that do not make a valid envelope (e.g. no command id), refused `Rejected`.
    pub invalid: u64,
}

/// Asset-local accord [`BusinessEventSink`]: every record is turned into its canonical
/// `hsk.studio.business_event@1` envelope ([`StudioBusinessEvent::from_record`]) and kept in a
/// bounded in-memory list. Memory only and process-local, like [`TransientHost`]; it stands in for
/// the host's Flight Recorder / EventLedger delivery (MT-36230) and proves nothing about host
/// acceptance. A record that makes no valid envelope is `Rejected`; a full sink refuses the new
/// event as `Saturated` (nothing is evicted); the same event id with an identical envelope is
/// accepted once (a replay), with a different envelope it is `Rejected` (STU-CON-013
/// immutable-envelope collision refusal; test
/// `transient_sink_is_bounded_idempotent_and_refuses_collisions`).
pub struct TransientBusinessEvents {
    capacity: usize,
    events: Vec<StudioBusinessEvent>,
    by_id: BTreeMap<String, usize>,
    stats: TransientEventStats,
}

impl TransientBusinessEvents {
    pub const DEFAULT_CAPACITY: usize = 4096;

    pub fn new(capacity: usize) -> Self {
        Self { capacity, events: Vec::new(), by_id: BTreeMap::new(), stats: TransientEventStats::default() }
    }

    /// The stored envelopes, in acceptance order.
    pub fn events(&self) -> &[StudioBusinessEvent] {
        &self.events
    }

    pub fn stats(&self) -> TransientEventStats {
        self.stats
    }
}

impl Default for TransientBusinessEvents {
    fn default() -> Self {
        Self::new(Self::DEFAULT_CAPACITY)
    }
}

impl BusinessEventSink for TransientBusinessEvents {
    fn try_emit(&mut self, record: &BusinessEventRecord) -> Result<(), BusinessEventDeliveryError> {
        let Ok(event) = StudioBusinessEvent::from_record(record) else {
            self.stats.invalid = self.stats.invalid.saturating_add(1);
            return Err(BusinessEventDeliveryError::Rejected);
        };
        if let Some(&index) = self.by_id.get(event.event_id()) {
            return if self.events.get(index) == Some(&event) {
                self.stats.replays = self.stats.replays.saturating_add(1);
                Ok(())
            } else {
                self.stats.collisions = self.stats.collisions.saturating_add(1);
                Err(BusinessEventDeliveryError::Rejected)
            };
        }
        if self.events.len() >= self.capacity {
            self.stats.saturated = self.stats.saturated.saturating_add(1);
            return Err(BusinessEventDeliveryError::Saturated);
        }
        let index = self.events.len();
        self.by_id.insert(event.event_id().to_owned(), index);
        self.events.push(event);
        self.stats.accepted = self.stats.accepted.saturating_add(1);
        Ok(())
    }
}
