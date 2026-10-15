//! Host-neutral integration bridge port schemas and executable in-memory adapter consumer. Concrete
//! AppState/SurrealDB/Argus/native adapters live later in Handshake product integration target and
//! are excluded from this assets workspace.
//!
//! This crate owns the public wire envelopes (`wire`), a byte-level host contract and a transient
//! process-local host (`host`), and the session-side adapter that implements the session's grant,
//! commit and job ports over those envelopes (`adapter`). It persists nothing.
#![forbid(unsafe_code)]

mod adapter;
mod ckc;
#[cfg(any(test, feature = "ckc-test-double"))]
mod ckc_double;
mod host;
mod wire;

pub use adapter::{BridgePorts, UNREACHABLE_RETRY_AFTER_MS};
pub use ckc::{
    ActorKind, AssetId, AssetMetadata, CKC_PORT_VERSION, CatalogField, CatalogOp, CatalogRule,
    CatalogValue, CkcActor, CkcCatalogPort, CkcError, CkcLimits, CkcReceipt, CollectionCommand,
    CollectionId, CollectionKind, ColorLabel, Criterion, Flag, KeywordCommand, KeywordPath,
    MetadataChange, MetadataMutation, Rating, SearchPage, SearchRequest, SidecarPlacement, SortKey,
    XmpRatingValue, XmpSidecarWrite,
};
#[cfg(any(test, feature = "ckc-test-double"))]
pub use ckc_double::{AssetFacts, CkcTestDouble, DoubleCollection};
pub use host::{DiagnosticRecord, HostBridge, HostConfig, TransientHost};
#[cfg(any(test, feature = "test-faults"))]
pub use host::Fault;
pub use wire::{
    ActionWire, ActorWire, BRIDGE_VERSION, Body, CommitDecision, CommitMsg, DenyReason,
    DiagnosticMsg, Envelope, ErrorReply, GrantDecision, JobDecision, JobDenyCode, Limits,
    LookupDecision, MAX_KEY_BYTES, MAX_REVISION_UPDATES, OperationWire, OutcomeWire, ReceiptWire,
    RejectCode, Reply, ReplyBody, RevisionUpdateWire, WireError, decode_envelope, decode_reply,
    diagnostic_outcome, encode_envelope, encode_reply, hex_decode, hex_encode,
};

/// V4-04 compile check: in a default build no fault-injection hook is a public item, so each
/// snippet below fails to compile (the `Fault` type is unresolved, `plan_fault` is not an item of
/// `TransientHost`). Each snippet compiles when the hook exists, so a public hook turns its doctest
/// red (checked with the item present under `--features test-faults`). The item is compiled out
/// with that feature, where the hooks exist.
///
/// ```compile_fail,E0432
/// use hsk_studio_kernel_bindings::Fault;
/// ```
///
/// ```compile_fail,E0433
/// let _ = hsk_studio_kernel_bindings::Fault::DropAck;
/// ```
///
/// ```compile_fail,E0599
/// let _ = hsk_studio_kernel_bindings::TransientHost::plan_fault;
/// ```
#[cfg(not(feature = "test-faults"))]
#[allow(dead_code)]
const FAULT_HOOKS_ARE_NOT_PUBLIC: () = ();

/// The CKC port double is test-only (STU-ASSET-004: no second shipped catalog store). In a
/// default build it is not an item of this crate, so this snippet fails to compile; with
/// `--features ckc-test-double` it compiles, and the item is compiled out there.
///
/// ```compile_fail,E0432
/// use hsk_studio_kernel_bindings::CkcTestDouble;
/// ```
#[cfg(not(feature = "ckc-test-double"))]
#[allow(dead_code)]
const CKC_DOUBLE_IS_NOT_SHIPPED: () = ();

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-KERNEL_BINDINGS","version":1,"bridge_version":1,"storage":"NONE: the transient host is memory only and process-local; nothing survives the process and no production storage or host wiring is claimed","api":"encode_envelope / decode_envelope / encode_reply / decode_reply (strict, canonical compact JSON); HostBridge::handle(bytes)->bytes; TransientHost{grant,revoke,seed_resource + independent inspection; plan_fault and Fault exist only with the non-default test-faults feature}; BridgePorts::new(host,actor,resource,epoch,limits).grants()/commits()/jobs() for SessionPorts","consumer":"kernel-bindings-consumer --descriptor | --roundtrip | --revoke | --drop-ack (needs --features test-faults) | --cancel","input":"Envelope{bridge_version,correlation_id,idempotency_key,actor,resource_id,expected_revision,lease_epoch,body} with body grant_check | commit | lookup | job_admit | job_release | diagnostic","output":"Reply{bridge_version,correlation_id,result} with result grant | commit(accepted|rejected|stale|stale_epoch|unknown) | lookup | job | ack | error","limits":{"max_bytes":4194304,"max_document_bytes":1048576,"max_key_bytes":128,"max_revision_updates":4096,"host_receipts":1024,"host_diagnostics":1024,"host_job_capacity":4},"strictness":"unknown fields, duplicate keys, a foreign bridge_version, trailing bytes, oversize input, invalid ids and non-lowercase hex are refused with distinct stable codes; canonical form is enforced by re-encoding the decoded value and requiring byte equality with the input, so interior whitespace, reordered keys, unicode escapes and omitted null fields are refused as non_canonical","fail_closed":"an absent or revoked grant denies and changes no state; job tickets are bound to the account, principal and resource they were admitted for (admission needs a standing grant) and a release by any other owner is refused as job denied not_ticket_owner with no state change; the lease epoch is host-owned: an older epoch is stale_epoch and a newer, never-issued one is rejected as epoch_not_issued and never adopted; a lost commit reply is outcome unknown (not rejected; undecodable and mismatched replies take the same code path but are not separately tested) and is resolved by lookup, not by blind re-application; commit rechecks the grant even for a known idempotency key","recovery":"outcome unknown: ask lookup(key); found adopts the receipt exactly once, absent allows a safe retry","manual":"Create a TransientHost, grant(actor,resource,Edit), seed_resource(resource,revision,epoch,document); build BridgePorts::new(Arc<host>,actor,resource,epoch,Limits::default()); pass .grants()/.commits()/.jobs() to SessionPorts::with_grants/with_commits/with_jobs; call Session::submit_keyed. Read TransientHost state for the independent result.","diagnostics":"every terminal commit result is also sent as one diagnostic envelope (operation, outcome, stable code; by construction no names, paths or document text); WireError::code() stable snake_case","proven_by":{"canonical_only_decoding":"tests/bridge_contract.rs only_canonical_bytes_decode","strictness_and_stable_codes":"wire_strictness_and_descriptor","fail_closed_grants":"fail_closed_on_absent_or_revoked_grant","unknown_outcome_on_lost_reply":"stale_epoch_conflict_and_unknown_outcome","job_ticket_owner":"job_tickets_are_released_only_by_their_owner","job_admission_grant":"job_admission_needs_a_standing_grant","host_owned_lease_epoch":"lease_epoch_is_issued_by_the_host_not_adopted_from_callers","hostile_sizes":"hostile_sizes_are_refused_typed","fault_hooks_only_under_test_faults":"tests/bridge_contract.rs fault_hooks_exist_only_under_the_test_faults_feature (source scan) and the compile_fail doctests on FAULT_HOOKS_ARE_NOT_PUBLIC in src/lib.rs"},"ckc_ports":{"version":1,"owner_boundary":"typed request/receipt contracts and the injected CkcCatalogPort trait only (STU-ASSET-004/010/019); no store, index, cache or rule evaluation ships in this crate; CKC (handshake_core atelier) implements the port in the integration target","commands":"get, search(SearchRequest{rule: CatalogRule All/Any/NoneOf/Match, sort keys, offset, limit}), mutate_metadata(MetadataMutation{request_id idempotency key, asset, expected_revision, changes SetRating/SetFlag/SetLabel/AddKeyword/RemoveKeyword}), keyword(DefineSynonyms), collection(Create manual|set, CreateSmart rule, AddMembers, RemoveMembers, SetRule, Delete), request_xmp_sidecar(XmpSidecarWrite: CKC writes the bytes)","vocabulary":"Rating Unrated|Stars 1..5, Flag Flagged|Unflagged|Rejected, ColorLabel Red|Yellow|Green|Blue|Purple (closed, Spec 10.10.4.5), KeywordPath root-first segments without |","validation":"every request validates ids (ASCII [A-Za-z0-9._:-], <= 128 bytes), text bounds and control characters, change/member/synonym/sort/page counts, rule depth and node count by an explicit stack, operator/field/value compatibility and range operands (test port_requests_are_validated_and_bounded)","host_obligations":"re-validate, authorize every call per actor (Unauthorized leaks no name or existence), enforce expected revision (StaleRevision{current}), replay the receipt of a repeated request_id, refuse a non-set parent and parent cycles, attribute operator vs model actors (STU-ASSET-021..023)","test_double":"CkcTestDouble exists only under cfg(test) or the non-default ckc-test-double feature (compile_fail doctest CKC_DOUBLE_IS_NOT_SHIPPED); its search semantics are its own, not CKC behaviour (test double_enforces_revision_idempotency_authorization_and_parents)","unverified":"the CKC host adapter does not exist yet; field and operator names are Studio vocabulary chosen here, not taken from the CKC packet (WP-KERNEL-005 contract not inspected)"},"later_host_adapters":[{"name":"AppState shared client","status":"NOT_IMPLEMENTED_HOST_ONLY","obligation":"receive the product's shared database client by reference; no second connection"},{"name":"ResourceBroker grant check","status":"NOT_IMPLEMENTED_HOST_ONLY","obligation":"authorize principal, resource and action on every check and every commit, replays included"},{"name":"Sandbox, validation catalog and PromotionGate decision","status":"NOT_IMPLEMENTED_HOST_ONLY","obligation":"model-authored proposals pass sandbox, validation and an Accepted promotion decision before any authority write; no self-approval"},{"name":"EventLedger atomic append","status":"NOT_IMPLEMENTED_HOST_ONLY","obligation":"commit, idempotency claim and event_ledger_event_id in one atomic transaction; the id is returned in the receipt host_ref"},{"name":"CRDT draft boundary","status":"NOT_IMPLEMENTED_HOST_ONLY","obligation":"drafts stay outside authority until promoted"},{"name":"Argus, Flight Recorder, internal diagnostics and Palmistry sinks","status":"NOT_IMPLEMENTED_HOST_ONLY","obligation":"deliver the diagnostic envelopes to the product's diagnostic surfaces"},{"name":"Job Runtime","status":"NOT_IMPLEMENTED_HOST_ONLY","obligation":"admit, cancel and status through the existing runtime; the bridge adds no queue"}]}"#;
