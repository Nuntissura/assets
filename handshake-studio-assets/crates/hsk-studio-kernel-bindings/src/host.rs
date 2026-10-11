//! The host side of the bridge: a byte-level `HostBridge` and a transient, process-local host
//! that really enforces what a canonical authority must (per-principal grants rechecked at
//! commit, lease-epoch fence, expected-revision compare, idempotency bound to principal,
//! resource and binding, monotonic receipts). It is memory only: nothing it holds survives the
//! process, and it is a stand-in for the concrete host adapters that live in the product.
use crate::wire::{
    ActionWire, Body, CommitDecision, CommitMsg, DenyReason, DiagnosticMsg, Envelope, ErrorReply,
    GrantDecision, JobDecision, Limits, LookupDecision, ReceiptWire, RejectCode, Reply, ReplyBody,
    decode_envelope, encode_reply, hex_decode,
};
use hsk_studio_accord::{ActorContext, DomainId};
use hsk_studio_session::Action;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Mutex, MutexGuard, PoisonError},
};

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

/// One-shot test faults, consumed by the next commit request in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The commit is applied, but the reply is lost (the caller sees no reply at all).
    DropAck,
    /// The principal's grant is revoked after the grant check and before the commit.
    RevokeBeforeCommit,
    /// The resource's lease epoch moves on before the commit.
    BumpEpoch,
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
    jobs: BTreeMap<u64, (u32, u64)>,
    next_ticket: u64,
    diagnostics: Vec<DiagnosticRecord>,
    faults: VecDeque<Fault>,
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

    pub fn plan_fault(&self, fault: Fault) {
        self.lock().faults.push_back(fault);
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
        self.lock()
            .jobs
            .values()
            .fold((0, 0), |(j, b), (jobs, bytes)| {
                (j.saturating_add(*jobs), b.saturating_add(*bytes))
            })
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
            Body::JobAdmit { jobs, bytes, .. } => self.job_admit(&mut state, *jobs, *bytes),
            Body::JobRelease { ticket } => {
                state.jobs.remove(ticket);
                ReplyBody::Ack
            }
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
                state.next_token += 1;
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

    fn job_admit(&self, state: &mut State, jobs: u32, bytes: u64) -> ReplyBody {
        let (used_jobs, used_bytes) = state
            .jobs
            .values()
            .fold((0u32, 0u64), |(j, b), (jobs, bytes)| {
                (j.saturating_add(*jobs), b.saturating_add(*bytes))
            });
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
        state.next_ticket += 1;
        state.jobs.insert(ticket, (jobs, bytes));
        ReplyBody::Job(JobDecision::Admitted { ticket })
    }

    /// Returns the decision and whether the acknowledgement is to be lost.
    fn commit(
        &self,
        state: &mut State,
        envelope: &Envelope,
        commit: &CommitMsg,
    ) -> (CommitDecision, bool) {
        let fault = state.faults.pop_front();
        let actor = &envelope.actor;
        let resource = envelope.resource_id.as_str();
        let edit_key: GrantKey = (
            actor.account_id.clone(),
            actor.principal_id.clone(),
            resource.to_owned(),
            ActionWire::Edit,
        );
        if fault == Some(Fault::RevokeBeforeCommit) {
            if state.grants.remove(&edit_key) {
                state.revoked.insert(edit_key.clone());
            }
            state.tokens.remove(&edit_key);
        }
        if fault == Some(Fault::BumpEpoch) {
            if let Some(r) = state.resources.get_mut(resource) {
                r.epoch += 1;
            }
        }
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
                (CommitDecision::Accepted(entry.receipt.clone()), fault == Some(Fault::DropAck))
            } else {
                reject(RejectCode::IdempotencyConflict)
            };
        }
        let Some(current) = state.resources.get(resource) else {
            return reject(RejectCode::ResourceUnknown);
        };
        // 3. Lease-epoch fence, then expected-revision compare.
        if envelope.lease_epoch < current.epoch {
            return (CommitDecision::StaleEpoch, false);
        }
        if envelope.expected_revision != current.revision {
            return (
                CommitDecision::Stale {
                    current_revision: current.revision,
                },
                false,
            );
        }
        if commit.successor_revision != envelope.expected_revision.saturating_add(1) {
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
        state.sequence += 1;
        let receipt = ReceiptWire {
            revision: commit.successor_revision,
            sequence: state.sequence,
            binding: commit.binding.clone(),
            host_ref: format!("transient-{}", state.sequence),
        };
        if let Some(r) = state.resources.get_mut(resource) {
            r.revision = commit.successor_revision;
            r.epoch = envelope.lease_epoch;
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
        (CommitDecision::Accepted(receipt), fault == Some(Fault::DropAck))
    }
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
        self.lock().requests += 1;
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
