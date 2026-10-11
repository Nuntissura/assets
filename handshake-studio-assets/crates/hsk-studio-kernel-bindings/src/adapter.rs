//! Session-side adapter: implements the session's `GrantPort`, `CommitPort` and `JobPort` by
//! encoding envelopes, calling a [`HostBridge`] and decoding replies. Fail closed everywhere: a
//! missing, undecodable or mismatched reply is a denied grant, a refused job, and for a commit
//! the distinct outcome `Unknown` (never `Rejected`: the host may have committed).
use crate::{
    host::HostBridge,
    wire::{
        ActionWire, ActorWire, Body, CommitDecision, CommitMsg, DenyReason, DiagnosticMsg,
        Envelope, JobDecision, Limits, LookupDecision, OperationWire, OutcomeWire, ReceiptWire,
        ReplyBody, RevisionUpdateWire, WireError, decode_reply, diagnostic_outcome,
        encode_envelope, hex_encode,
    },
};
use hsk_studio_accord::{ActorContext, DomainId};
use hsk_studio_observe::{FailureCode, Outcome};
use hsk_studio_session::{
    Action, CommitOutcome, CommitPort, CommitReceipt, CommitRequest, Cost, GrantError, GrantPort,
    GrantToken, IdempotencyKey, JobPort, JobRefused, JobScope, JobTicket, Lookup,
};
use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicU64, Ordering},
};

/// How long a caller is told to wait when the host gave no usable answer to a job request.
pub const UNREACHABLE_RETRY_AFTER_MS: u64 = 1000;
const SENT_LOG_CAP: usize = 64;

struct Shared<H> {
    host: Arc<H>,
    actor: ActorContext,
    resource: DomainId,
    epoch: u64,
    limits: Limits,
    correlation: AtomicU64,
    sent_total: AtomicU64,
    sent: Mutex<Vec<Vec<u8>>>,
}

impl<H: HostBridge> Shared<H> {
    /// One request/reply exchange. `None`: nothing was sent, or no usable matching reply came
    /// back.
    fn exchange(
        &self,
        actor: &ActorContext,
        resource: &DomainId,
        expected_revision: u64,
        key: Option<&str>,
        body: Body,
    ) -> Exchange {
        let correlation = self.correlation.fetch_add(1, Ordering::Relaxed) + 1;
        let envelope = Envelope {
            bridge_version: crate::wire::BRIDGE_VERSION,
            correlation_id: correlation,
            idempotency_key: key.map(str::to_owned),
            actor: ActorWire::from_context(actor),
            resource_id: resource.as_str().to_owned(),
            expected_revision,
            lease_epoch: self.epoch,
            body,
        };
        let bytes = match encode_envelope(&envelope, &self.limits) {
            Ok(bytes) => bytes,
            Err(_) => return Exchange::NotSent,
        };
        self.sent_total.fetch_add(1, Ordering::Relaxed);
        {
            let mut sent = self.sent.lock().unwrap_or_else(PoisonError::into_inner);
            if sent.len() == SENT_LOG_CAP {
                sent.remove(0);
            }
            sent.push(bytes.clone());
        }
        let raw = self.host.handle(&bytes);
        match decode_reply(&raw, &self.limits) {
            Ok(reply) if reply.correlation_id == correlation => Exchange::Reply(reply.result),
            _ => Exchange::NoReply,
        }
    }

    /// Best-effort terminal report; it can never change the result it reports.
    fn report(&self, revision: u64, outcome: Outcome, code: Option<&str>) {
        let Some(outcome) = OutcomeWire::from_observe(outcome) else {
            return;
        };
        let _ = self.exchange(
            &self.actor,
            &self.resource,
            revision,
            None,
            Body::Diagnostic(DiagnosticMsg {
                operation: OperationWire::Commit,
                outcome,
                code: code.map(str::to_owned),
                revision,
            }),
        );
    }
}

enum Exchange {
    NotSent,
    NoReply,
    Reply(ReplyBody),
}

/// Session-facing ports over one host. Hand [`BridgePorts::grants`], [`BridgePorts::commits`] and
/// [`BridgePorts::jobs`] to `SessionPorts`.
pub struct BridgePorts<H> {
    shared: Arc<Shared<H>>,
}

impl<H> Clone for BridgePorts<H> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<H: HostBridge + 'static> BridgePorts<H> {
    /// `actor`, `resource` and `epoch` are the session's own (the commit and job ports are not
    /// given them per call).
    pub fn new(
        host: Arc<H>,
        actor: ActorContext,
        resource: DomainId,
        epoch: u64,
        limits: Limits,
    ) -> Result<Self, WireError> {
        if !limits.valid() {
            return Err(WireError::InvalidLimits);
        }
        Ok(Self {
            shared: Arc::new(Shared {
                host,
                actor,
                resource,
                epoch,
                limits,
                correlation: AtomicU64::new(0),
                sent_total: AtomicU64::new(0),
                sent: Mutex::new(Vec::new()),
            }),
        })
    }

    pub fn grants(&self) -> Box<dyn GrantPort> {
        Box::new(BridgeGrants {
            shared: Arc::clone(&self.shared),
        })
    }

    pub fn commits(&self) -> Box<dyn CommitPort> {
        Box::new(BridgeCommits {
            shared: Arc::clone(&self.shared),
        })
    }

    pub fn jobs(&self) -> Arc<dyn JobPort> {
        Arc::new(BridgeJobs {
            shared: Arc::clone(&self.shared),
        })
    }

    /// Requests that left this adapter (cancelled-before-send operations add none).
    pub fn sent_count(&self) -> u64 {
        self.shared.sent_total.load(Ordering::Relaxed)
    }

    /// The most recent request bytes (bounded), oldest first.
    pub fn sent_requests(&self) -> Vec<Vec<u8>> {
        self.shared
            .sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

struct BridgeGrants<H> {
    shared: Arc<Shared<H>>,
}

impl<H: HostBridge + 'static> GrantPort for BridgeGrants<H> {
    fn check(
        &self,
        actor: &ActorContext,
        resource: &DomainId,
        action: Action,
    ) -> Result<GrantToken, GrantError> {
        match self.shared.exchange(
            actor,
            resource,
            0,
            None,
            Body::GrantCheck {
                action: ActionWire::from(action),
            },
        ) {
            Exchange::Reply(ReplyBody::Grant(decision)) if decision.granted => {
                Ok(GrantToken(decision.token))
            }
            Exchange::Reply(ReplyBody::Grant(decision)) => match decision.reason {
                Some(DenyReason::Revoked) => Err(GrantError::Revoked),
                _ => Err(GrantError::Missing),
            },
            // No usable answer is a denial: an unreachable authority grants nothing.
            _ => Err(GrantError::Missing),
        }
    }
}

struct BridgeCommits<H> {
    shared: Arc<Shared<H>>,
}

fn receipt_from_wire(receipt: ReceiptWire) -> CommitReceipt {
    CommitReceipt {
        revision: receipt.revision,
        sequence: receipt.sequence,
        binding: receipt.binding,
    }
}

fn updates_to_wire<T: serde::Serialize>(updates: &[T]) -> Option<Vec<RevisionUpdateWire>> {
    updates
        .iter()
        .map(|update| {
            let value = serde_json::to_value(update).ok()?;
            Some(RevisionUpdateWire {
                address: value.get("address")?.to_string(),
                previous_revision: value.get("previous_revision")?.as_u64()?,
                revision: value.get("revision")?.as_u64()?,
            })
        })
        .collect()
}

impl<H: HostBridge + 'static> CommitPort for BridgeCommits<H> {
    fn commit(&self, request: &CommitRequest<'_>) -> CommitOutcome {
        let Some(revision_updates) = updates_to_wire(request.revision_updates) else {
            return CommitOutcome::Rejected("wire_invalid");
        };
        let body = Body::Commit(CommitMsg {
            grant_token: request.grant.0,
            successor_revision: request.successor_revision,
            binding: request.binding.to_owned(),
            document_hex: hex_encode(request.successor_document),
            revision_updates,
        });
        let exchange = self.shared.exchange(
            &self.shared.actor,
            request.resource,
            request.expected_revision,
            Some(request.key.as_str()),
            body,
        );
        let (outcome, reported) = match exchange {
            // Refused before anything left this process: a definitive no-op.
            Exchange::NotSent => return CommitOutcome::Rejected("wire_invalid"),
            Exchange::Reply(ReplyBody::Commit(decision)) => {
                let reported = diagnostic_outcome(&ReplyBody::Commit(decision.clone()));
                let outcome = match decision {
                    CommitDecision::Accepted(receipt) => {
                        CommitOutcome::Committed(receipt_from_wire(receipt))
                    }
                    CommitDecision::Rejected { code } => CommitOutcome::Rejected(code.as_str()),
                    CommitDecision::Stale { current_revision } => {
                        CommitOutcome::Stale { current_revision }
                    }
                    CommitDecision::StaleEpoch => CommitOutcome::StaleEpoch,
                    CommitDecision::Unknown => CommitOutcome::Unknown,
                };
                (outcome, reported)
            }
            // Lost, malformed, mismatched or foreign reply: the host may have committed.
            Exchange::Reply(_) | Exchange::NoReply => {
                (CommitOutcome::Unknown, Outcome::Failure(FailureCode::Unavailable))
            }
        };
        self.shared
            .report(request.expected_revision, reported, outcome_code(&outcome));
        outcome
    }

    fn lookup(&self, key: &IdempotencyKey) -> Lookup {
        match self.shared.exchange(
            &self.shared.actor,
            &self.shared.resource,
            0,
            Some(key.as_str()),
            Body::Lookup,
        ) {
            Exchange::Reply(ReplyBody::Lookup(LookupDecision::Found(receipt))) => {
                Lookup::Found(receipt_from_wire(receipt))
            }
            Exchange::Reply(ReplyBody::Lookup(LookupDecision::Absent)) => Lookup::Absent,
            _ => Lookup::Unknown,
        }
    }
}

fn outcome_code(outcome: &CommitOutcome) -> Option<&'static str> {
    match outcome {
        CommitOutcome::Committed(_) => None,
        CommitOutcome::Rejected(code) => Some(code),
        CommitOutcome::Stale { .. } => Some("stale_revision"),
        CommitOutcome::StaleEpoch => Some("stale_epoch"),
        CommitOutcome::Unknown => Some("outcome_unknown"),
    }
}

struct BridgeJobs<H> {
    shared: Arc<Shared<H>>,
}

impl<H: HostBridge + 'static> JobPort for BridgeJobs<H> {
    fn admit(&self, scope: &JobScope<'_>, cost: Cost) -> Result<JobTicket, JobRefused> {
        match self.shared.exchange(
            &self.shared.actor,
            scope.resource,
            0,
            None,
            Body::JobAdmit {
                action: ActionWire::from(scope.action),
                jobs: cost.jobs,
                bytes: cost.bytes,
            },
        ) {
            Exchange::Reply(ReplyBody::Job(JobDecision::Admitted { ticket })) => {
                Ok(JobTicket(ticket))
            }
            Exchange::Reply(ReplyBody::Job(JobDecision::Refused { retry_after_ms })) => {
                Err(JobRefused { retry_after_ms })
            }
            _ => Err(JobRefused {
                retry_after_ms: UNREACHABLE_RETRY_AFTER_MS,
            }),
        }
    }

    fn release(&self, ticket: JobTicket) {
        let _ = self.shared.exchange(
            &self.shared.actor,
            &self.shared.resource,
            0,
            None,
            Body::JobRelease { ticket: ticket.0 },
        );
    }
}
