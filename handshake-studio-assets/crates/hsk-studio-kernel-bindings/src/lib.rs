//! Host-neutral integration bridge port schemas and executable in-memory adapter consumer. Concrete
//! AppState/SurrealDB/Argus/native adapters live later in Handshake product integration target and
//! are excluded from this assets workspace.
//!
//! This crate owns the public wire envelopes (`wire`), a byte-level host contract and a transient
//! process-local host (`host`), and the session-side adapter that implements the session's grant,
//! commit and job ports over those envelopes (`adapter`). It persists nothing.
#![forbid(unsafe_code)]

mod adapter;
mod host;
mod wire;

pub use adapter::{BridgePorts, UNREACHABLE_RETRY_AFTER_MS};
pub use host::{DiagnosticRecord, Fault, HostBridge, HostConfig, TransientHost};
pub use wire::{
    ActionWire, ActorWire, BRIDGE_VERSION, Body, CommitDecision, CommitMsg, DenyReason,
    DiagnosticMsg, Envelope, ErrorReply, GrantDecision, JobDecision, Limits, LookupDecision,
    MAX_KEY_BYTES, MAX_REVISION_UPDATES, OperationWire, OutcomeWire, ReceiptWire, RejectCode,
    Reply, ReplyBody, RevisionUpdateWire, WireError, decode_envelope, decode_reply,
    diagnostic_outcome, encode_envelope, encode_reply, hex_decode, hex_encode,
};

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-KERNEL_BINDINGS","version":1,"bridge_version":1,"storage":"NONE: the transient host is memory only and process-local; nothing survives the process and no production storage or host wiring is claimed","api":"encode_envelope / decode_envelope / encode_reply / decode_reply (strict, canonical compact JSON); HostBridge::handle(bytes)->bytes; TransientHost{grant,revoke,seed_resource,plan_fault + independent inspection}; BridgePorts::new(host,actor,resource,epoch,limits).grants()/commits()/jobs() for SessionPorts","consumer":"kernel-bindings-consumer --descriptor | --roundtrip | --revoke | --drop-ack | --cancel","input":"Envelope{bridge_version,correlation_id,idempotency_key,actor,resource_id,expected_revision,lease_epoch,body} with body grant_check | commit | lookup | job_admit | job_release | diagnostic","output":"Reply{bridge_version,correlation_id,result} with result grant | commit(accepted|rejected|stale|stale_epoch|unknown) | lookup | job | ack | error","limits":{"max_bytes":4194304,"max_document_bytes":1048576,"max_key_bytes":128,"max_revision_updates":4096,"host_receipts":1024,"host_diagnostics":1024,"host_job_capacity":4},"strictness":"unknown fields, duplicate keys, a foreign bridge_version, trailing bytes, oversize input, invalid ids and non-lowercase hex are refused with distinct stable codes","fail_closed":"an absent or revoked grant denies and changes no state; a lost or undecodable commit reply is outcome unknown (never rejected) and is resolved by lookup, never by blind re-application; commit rechecks the grant even for a known idempotency key","recovery":"outcome unknown: ask lookup(key); found adopts the receipt exactly once, absent allows a safe retry","manual":"Create a TransientHost, grant(actor,resource,Edit), seed_resource(resource,revision,epoch,document); build BridgePorts::new(Arc<host>,actor,resource,epoch,Limits::default()); pass .grants()/.commits()/.jobs() to SessionPorts::with_grants/with_commits/with_jobs; call Session::submit_keyed. Read TransientHost state for the independent result.","diagnostics":"every terminal commit result is also sent as one diagnostic envelope (operation, outcome, stable code; no names, paths or document text); WireError::code() stable snake_case","later_host_adapters":[{"name":"AppState shared client","status":"NOT_IMPLEMENTED_HOST_ONLY","obligation":"receive the product's shared database client by reference; no second connection"},{"name":"ResourceBroker grant check","status":"NOT_IMPLEMENTED_HOST_ONLY","obligation":"authorize principal, resource and action on every check and every commit, replays included"},{"name":"Sandbox, validation catalog and PromotionGate decision","status":"NOT_IMPLEMENTED_HOST_ONLY","obligation":"model-authored proposals pass sandbox, validation and an Accepted promotion decision before any authority write; no self-approval"},{"name":"EventLedger atomic append","status":"NOT_IMPLEMENTED_HOST_ONLY","obligation":"commit, idempotency claim and event_ledger_event_id in one atomic transaction; the id is returned in the receipt host_ref"},{"name":"CRDT draft boundary","status":"NOT_IMPLEMENTED_HOST_ONLY","obligation":"drafts stay outside authority until promoted"},{"name":"Argus, Flight Recorder, internal diagnostics and Palmistry sinks","status":"NOT_IMPLEMENTED_HOST_ONLY","obligation":"deliver the diagnostic envelopes to the product's diagnostic surfaces"},{"name":"Job Runtime","status":"NOT_IMPLEMENTED_HOST_ONLY","obligation":"admit, cancel and status through the existing runtime; the bridge adds no queue"}]}"#;
