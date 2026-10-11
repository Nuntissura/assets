//! Envelope in -> real `Session` operation -> envelope out, against the transient host. The
//! oracle is the host's own state (read through its inspection API) and the independently parsed
//! document bytes it holds, never the adapter's or the session's return values alone.
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio::{self as folio, Inspection};
use hsk_studio_kernel_bindings::*;
use hsk_studio_observe::{DeliveryClass, DeliveryError, FailureCode, Outcome, SinkPort};
use hsk_studio_session::{
    Action, Applied, CHRONICLE_PROVIDER_ID, Failed, IdempotencyKey, ReportStatus, Session,
    SessionConfig, SessionPorts,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("case {name} exceeded declared 30-second deadline")
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("case {name} lost deadline worker result")
        }
    }
}

// ----- fixtures --------------------------------------------------------------------------------

/// A package written by the session's own consumer (revision 9, layer 1 named
/// `renamed-by-session`); checked in so these tests need no package writer.
const PACKAGE: &[u8] = include_bytes!("fixtures/doc-rev9.handshake");
const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";
const LAYER_1: &str = "SLYR-019abcde-0000-7000-8000-000000000001";
const EPOCH: u64 = 3;
const START: u64 = 9;

struct Granted;
impl folio::Resolver for Granted {
    fn primitive(&self, _: &str, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn tile(&self, _: &folio::TileRef) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn format_supported(&self, f: &str) -> bool {
        f == "caller-selected-format"
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}

#[derive(Clone, Default)]
struct Frames(Arc<Mutex<u32>>);
impl Frames {
    fn count(&self) -> u32 {
        *self.0.lock().unwrap()
    }
}
struct TestSink(Frames);
impl SinkPort for TestSink {
    fn try_send(&mut self, class: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        assert!(class == DeliveryClass::Terminal && !bytes.is_empty());
        *self.0.0.lock().unwrap() += 1;
        Ok(())
    }
}

/// The transient host plus an optional "cancel once the commit has been handled" hook.
#[derive(Default)]
struct TestHost {
    inner: TransientHost,
    cancel_after_commit: Mutex<Option<CancellationToken>>,
}
impl HostBridge for TestHost {
    fn handle(&self, request: &[u8]) -> Vec<u8> {
        let reply = self.inner.handle(request);
        if let Ok(envelope) = decode_envelope(request, &Limits::default())
            && matches!(envelope.body, Body::Commit(_))
            && let Some(token) = self.cancel_after_commit.lock().unwrap().as_ref()
        {
            token.cancel();
        }
        reply
    }
}

fn actor() -> ActorContext {
    ActorContext::new(
        "account",
        "principal",
        "owner",
        "owner-principal",
        "space",
        "session",
    )
    .unwrap()
}

fn token() -> CancellationToken {
    CancellationToken::default()
}

struct Rig {
    host: Arc<TestHost>,
    bridge: BridgePorts<TestHost>,
    session: Session,
    resource: DomainId,
    frames: Frames,
}

fn rig(grant: bool) -> Rig {
    let (actor, resource, frames) = (actor(), DomainId::parse(DOC).unwrap(), Frames::default());
    let host = Arc::new(TestHost::default());
    let bridge = BridgePorts::new(
        Arc::clone(&host),
        actor.clone(),
        resource.clone(),
        EPOCH,
        Limits::default(),
    )
    .unwrap();
    let mut config = SessionConfig::new(actor.clone());
    config.epoch = EPOCH;
    let ports = SessionPorts::new(Box::new(TestSink(frames.clone())))
        .with_resolver(Box::new(Granted))
        .with_grants(bridge.grants())
        .with_commits(bridge.commits())
        .with_jobs(bridge.jobs());
    let session = Session::open_bytes(PACKAGE, config, ports, &token()).unwrap();
    assert_eq!(session.document_revision(), START);
    host.inner
        .seed_resource(&resource, START, EPOCH, session.snapshot().encoded_bytes());
    if grant {
        host.inner.grant(&actor, &resource, Action::Edit);
    }
    Rig {
        host,
        bridge,
        session,
        resource,
        frames,
    }
}

fn rename_patch(from: &str, to: &str) -> Vec<u8> {
    let address = json!({"target":{"node_kind":"layer","node_id":LAYER_1},"property":"name"});
    serde_json::to_vec(&json!({
        "transport_version":1,"document_id":DOC,"base_revision":START,"command_id":"bridge-edit-1","correlation_id":1,
        "actor":{"account_id":"account","principal_id":"principal","owner_account_id":"owner","owner_principal_id":"owner-principal","access_space_id":"space","session_id":"session"},
        "reads":[{"address":address,"expected_revision":0,"expected_value":{"value_kind":"string","value":from}}],
        "writes":[{"address":address,"value":{"value_kind":"string","value":to}}]
    }))
    .unwrap()
}

const FROM: &str = "renamed-by-session";
const TO: &str = "renamed-by-bridge";

fn key(name: &str) -> IdempotencyKey {
    IdempotencyKey::parse(name).unwrap()
}

fn submit(rig: &mut Rig, key: &IdempotencyKey, cancel: &CancellationToken) -> Result<Applied, Failed> {
    rig.session
        .submit_keyed(key, CHRONICLE_PROVIDER_ID, &rename_patch(FROM, TO), START, cancel)
}

fn kind(body: &Body) -> &'static str {
    match body {
        Body::GrantCheck { .. } => "grant_check",
        Body::Commit(_) => "commit",
        Body::Lookup => "lookup",
        Body::JobAdmit { .. } => "job_admit",
        Body::JobRelease { .. } => "job_release",
        Body::Diagnostic(_) => "diagnostic",
    }
}

fn sent(rig: &Rig) -> Vec<Envelope> {
    rig.bridge
        .sent_requests()
        .iter()
        .map(|bytes| decode_envelope(bytes, &Limits::default()).unwrap())
        .collect()
}

fn sent_kinds(rig: &Rig) -> Vec<&'static str> {
    sent(rig).iter().map(|e| kind(&e.body)).collect()
}

fn delivered(report: ReportStatus) -> Outcome {
    match report {
        ReportStatus::Delivered(receipt) => receipt.outcome,
        other => panic!("report not delivered: {other:?}"),
    }
}

/// Name of layer 1 and revision of the document bytes the host holds, parsed independently.
fn host_document(rig: &Rig) -> (u64, String) {
    let bytes = rig.host.inner.document_bytes(&rig.resource).unwrap();
    let inspection = folio::inspect_bytes(
        &bytes,
        folio::Budget::default(),
        &token(),
        &Granted,
    )
    .unwrap();
    let Inspection::Editable(snapshot) = inspection else {
        panic!("host document became read-only");
    };
    let document = snapshot.document();
    (document.revision, document.layers[0].name.clone())
}

fn only_diagnostic(rig: &Rig) -> DiagnosticMsg {
    let all = rig.host.inner.diagnostics();
    assert_eq!(all.len(), 1, "exactly one diagnostic envelope per terminal commit result");
    all[0].message.clone()
}

// ----- tests -----------------------------------------------------------------------------------

#[test]
fn roundtrip_envelopes_through_session_and_idempotent_replay() {
    bounded_case("roundtrip", || {
        let mut rig = rig(true);
        let first = submit(&mut rig, &key("k-1"), &token()).unwrap();
        assert_eq!((first.previous_revision, first.revision), (START, START + 1));
        let receipt = first.commit.clone().expect("authority receipt");
        assert_eq!((receipt.revision, receipt.sequence), (START + 1, 1));
        assert!(!first.replayed);
        assert_eq!(delivered(first.report), Outcome::Success);

        // Oracle: the host's own state and the document bytes it holds.
        let host = &rig.host.inner;
        assert_eq!(host.committed_revision(&rig.resource), Some(START + 1));
        assert_eq!((host.receipts().len(), host.idempotency_len()), (1, 1));
        assert_eq!(host.receipts()[0].binding, receipt.binding);
        assert_eq!(host_document(&rig), (START + 1, TO.to_owned()));
        assert_eq!(host.jobs_in_flight(), (0, 0), "job ticket released on exit");

        // Protocol order and a byte-level golden for the first request.
        assert_eq!(
            sent_kinds(&rig),
            ["grant_check", "job_admit", "commit", "diagnostic", "job_release"]
        );
        let requests = rig.bridge.sent_requests();
        assert_eq!(
            String::from_utf8(requests[0].clone()).unwrap(),
            r#"{"bridge_version":1,"correlation_id":1,"idempotency_key":null,"actor":{"account_id":"account","principal_id":"principal","owner_account_id":"owner","owner_principal_id":"owner-principal","access_space_id":"space","session_id":"session"},"resource_id":"SDOC-019abcde-0000-7000-8000-000000000001","expected_revision":0,"lease_epoch":3,"body":{"grant_check":{"action":"edit"}}}"#
        );
        let commit = sent(&rig).remove(2);
        assert_eq!(
            (commit.idempotency_key.as_deref(), commit.expected_revision, commit.lease_epoch),
            (Some("k-1"), START, EPOCH)
        );
        assert_eq!(encode_envelope(&commit, &Limits::default()).unwrap(), requests[2]);
        let diagnostic = only_diagnostic(&rig);
        assert_eq!(diagnostic.outcome, OutcomeWire::Success);
        assert_eq!(diagnostic.code, None);

        // Session-level replay: same key and payload -> the stored receipt, after a FRESH grant
        // check, with nothing sent to commit and nothing applied again.
        let replay = submit(&mut rig, &key("k-1"), &token()).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.commit, Some(receipt.clone()));
        assert_eq!(sent_kinds(&rig).iter().filter(|k| **k == "grant_check").count(), 2);
        assert_eq!(sent_kinds(&rig).iter().filter(|k| **k == "commit").count(), 1);

        // Host-level replay: the very same commit bytes answer the same receipt, applied once.
        let reply = decode_reply(&rig.host.handle(&requests[2]), &Limits::default()).unwrap();
        assert_eq!(
            reply.result,
            ReplyBody::Commit(CommitDecision::Accepted(rig.host.inner.receipts()[0].clone()))
        );
        assert_eq!(rig.host.inner.receipts().len(), 1);
        assert_eq!(rig.host.inner.committed_revision(&rig.resource), Some(START + 1));

        // Reauthorization is mandatory even for a known receipt.
        rig.host.inner.revoke(&actor(), &rig.resource, Action::Edit);
        let denied = submit(&mut rig, &key("k-1"), &token()).unwrap_err();
        assert_eq!(denied.code(), "grant_revoked");
        assert_eq!(rig.frames.count(), 4, "open + three submits: one terminal frame each");
    });
}

#[test]
fn fail_closed_on_absent_or_revoked_grant() {
    bounded_case("fail_closed", || {
        let mut rig = rig(false);
        // No grant at all: denied, nothing committed, nothing but the grant check sent.
        let denied = submit(&mut rig, &key("k-1"), &token()).unwrap_err();
        assert_eq!(denied.code(), "grant_missing");
        assert_eq!(delivered(denied.report), Outcome::Failure(FailureCode::Validation));
        assert_eq!(sent_kinds(&rig), ["grant_check"]);
        let host_arc = Arc::clone(&rig.host);
        let host = &host_arc.inner;
        assert_eq!(host.committed_revision(&rig.resource), Some(START));
        assert_eq!((host.receipts().len(), host.idempotency_len()), (0, 0));

        // Revoked between the grant check and the commit: rejected, no state change anywhere.
        host.grant(&actor(), &rig.resource, Action::Edit);
        host.plan_fault(Fault::RevokeBeforeCommit);
        let revoked = submit(&mut rig, &key("k-1"), &token()).unwrap_err();
        assert_eq!(revoked.code(), "grant_revoked");
        assert_eq!(delivered(revoked.report), Outcome::Failure(FailureCode::Validation));
        assert_eq!(rig.session.document_revision(), START);
        assert_eq!(rig.session.receipt_count(), 0, "a rejected key stores no receipt");
        assert_eq!(host.committed_revision(&rig.resource), Some(START));
        assert_eq!((host.receipts().len(), host.idempotency_len()), (0, 0));
        assert_eq!(host.jobs_in_flight(), (0, 0));
        let diagnostic = only_diagnostic(&rig);
        assert_eq!(diagnostic.outcome, OutcomeWire::FailureValidation);
        assert_eq!(diagnostic.code.as_deref(), Some("grant_revoked"));

        // The retry re-checks the grant (a new one stands now) instead of replaying anything.
        host.grant(&actor(), &rig.resource, Action::Edit);
        let before = rig.bridge.sent_count();
        let applied = submit(&mut rig, &key("k-1"), &token()).unwrap();
        assert!(!applied.replayed);
        assert_eq!(sent(&rig)[before as usize].body, Body::GrantCheck { action: ActionWire::Edit });
        assert_eq!(host.committed_revision(&rig.resource), Some(START + 1));
        assert_eq!(host.receipts().len(), 1);
    });
}

#[test]
fn stale_epoch_conflict_and_unknown_outcome() {
    bounded_case("stale_conflict_unknown", || {
        // Lease epoch moved on at the host: StaleEpoch, nothing applied.
        let mut a = rig(true);
        a.host.inner.plan_fault(Fault::BumpEpoch);
        let stale_epoch = submit(&mut a, &key("k-1"), &token()).unwrap_err();
        assert_eq!(stale_epoch.code(), "stale_epoch");
        assert_eq!(a.host.inner.committed_revision(&a.resource), Some(START));
        assert_eq!(a.host.inner.lease_epoch(&a.resource), Some(EPOCH + 1));
        assert_eq!(a.host.inner.receipts().len(), 0);

        // Host holds a newer revision than the session expected: Stale, current is reported.
        let mut b = rig(true);
        b.host.inner.seed_resource(&b.resource, START + 3, EPOCH, b"newer");
        let stale = submit(&mut b, &key("k-1"), &token()).unwrap_err();
        assert_eq!(stale.code(), "stale_revision");
        assert_eq!(b.host.inner.document_bytes(&b.resource).unwrap(), b"newer");
        assert_eq!(b.host.inner.receipts().len(), 0);

        // Same key, changed binding, sent straight to the host: conflict, nothing disclosed.
        let mut c = rig(true);
        submit(&mut c, &key("k-1"), &token()).unwrap();
        let mut forged = sent(&c).remove(2);
        let Body::Commit(commit) = &mut forged.body else {
            panic!("third request is the commit");
        };
        commit.binding = "0".repeat(64);
        let reply = c
            .host
            .handle(&encode_envelope(&forged, &Limits::default()).unwrap());
        assert_eq!(
            decode_reply(&reply, &Limits::default()).unwrap().result,
            ReplyBody::Commit(CommitDecision::Rejected {
                code: RejectCode::IdempotencyConflict
            })
        );
        assert_eq!(c.host.inner.receipts().len(), 1);

        // Lost acknowledgement: the host applied the edit, the session sees Unknown (not
        // Rejected), lookup rediscovers the receipt, and the edit is applied exactly once.
        let mut d = rig(true);
        d.host.inner.plan_fault(Fault::DropAck);
        let unknown = submit(&mut d, &key("k-1"), &token()).unwrap_err();
        assert_eq!(unknown.code(), "outcome_unknown");
        assert_eq!(delivered(unknown.report), Outcome::Failure(FailureCode::Unavailable));
        assert_eq!(d.session.document_revision(), START, "not adopted before the receipt is known");
        assert_eq!(d.host.inner.committed_revision(&d.resource), Some(START + 1));
        assert_eq!(d.session.pending(), ["k-1"]);
        let diagnostic = only_diagnostic(&d);
        assert_eq!(diagnostic.outcome, OutcomeWire::FailureUnavailable);
        assert_eq!(diagnostic.code.as_deref(), Some("outcome_unknown"));
        let report = d.session.reconcile(&token()).unwrap();
        assert_eq!((report.adopted, report.cleared, report.still_unknown), (1, 0, 0));
        assert_eq!(d.session.document_revision(), START + 1);
        assert_eq!(d.host.inner.receipts().len(), 1, "applied exactly once");
        assert_eq!(sent_kinds(&d).iter().filter(|k| **k == "commit").count(), 1);
        assert!(sent_kinds(&d).contains(&"lookup"));
        assert_eq!(host_document(&d), (START + 1, TO.to_owned()));
    });
}

#[test]
fn cancel_semantics_and_diagnostics() {
    bounded_case("cancel_and_diagnostics", || {
        // Cancel before send: not one byte reaches the host, no diagnostic, one Canceled frame.
        let mut a = rig(true);
        let canceled = token();
        canceled.cancel();
        let frames_before = a.frames.count();
        let failed = submit(&mut a, &key("k-1"), &canceled).unwrap_err();
        assert_eq!(failed.code(), "canceled");
        assert_eq!(delivered(failed.report), Outcome::Canceled);
        assert_eq!(a.frames.count(), frames_before + 1);
        assert_eq!(a.bridge.sent_count(), 0);
        assert_eq!(a.host.inner.requests_seen(), 0);
        assert!(a.host.inner.diagnostics().is_empty());

        // Cancel arriving after the host accepted: the committed receipt is returned, the edit
        // stands on both sides, and exactly one Success diagnostic was reported.
        let mut b = rig(true);
        let late = token();
        *b.host.cancel_after_commit.lock().unwrap() = Some(late.clone());
        let applied = submit(&mut b, &key("k-1"), &late).unwrap();
        assert!(applied.cancel_too_late);
        assert_eq!(applied.commit.as_ref().map(|r| r.revision), Some(START + 1));
        assert_eq!(delivered(applied.report), Outcome::Success);
        assert_eq!(b.session.document_revision(), START + 1);
        assert_eq!(b.host.inner.committed_revision(&b.resource), Some(START + 1));
        assert_eq!(b.host.inner.receipts().len(), 1);
        let diagnostic = only_diagnostic(&b);
        assert_eq!(diagnostic.outcome, OutcomeWire::Success);
        assert_eq!(
            diagnostic_outcome(&ReplyBody::Commit(CommitDecision::StaleEpoch)),
            Outcome::Failure(FailureCode::Validation)
        );
    });
}

#[test]
fn wire_strictness_and_descriptor() {
    bounded_case("wire_strictness", || {
        let limits = Limits::default();
        let envelope = Envelope {
            bridge_version: BRIDGE_VERSION,
            correlation_id: 7,
            idempotency_key: Some("k-1".into()),
            actor: ActorWire::from_context(&actor()),
            resource_id: DOC.into(),
            expected_revision: START,
            lease_epoch: EPOCH,
            body: Body::Commit(CommitMsg {
                grant_token: 1,
                successor_revision: START + 1,
                binding: "b".repeat(64),
                document_hex: hex_encode(b"{}"),
                revision_updates: vec![RevisionUpdateWire {
                    address: "{}".into(),
                    previous_revision: 0,
                    revision: 1,
                }],
            }),
        };
        let good = encode_envelope(&envelope, &limits).unwrap();
        let text = String::from_utf8(good.clone()).unwrap();
        assert_eq!(decode_envelope(&good, &limits).unwrap(), envelope);
        assert_eq!(encode_envelope(&decode_envelope(&good, &limits).unwrap(), &limits).unwrap(), good);
        assert!(text.contains(r#""document_hex":"7b7d""#));

        let code = |bytes: &[u8]| decode_envelope(bytes, &limits).unwrap_err().code();
        let tight = Limits {
            max_bytes: 100,
            ..limits
        };
        let cases: Vec<(&str, &str, &str)> = vec![
            ("unknown_field", "\"bridge_version\":1", "\"bridge_version\":1,\"extra\":1"),
            ("wrong_version", "\"bridge_version\":1", "\"bridge_version\":2"),
            ("duplicate_key", "\"bridge_version\":1", "\"bridge_version\":1,\"bridge_version\":1"),
            ("duplicate_key", "\"account_id\":\"account\"", "\"account_id\":\"account\",\"account_id\":\"x\""),
            ("invalid_resource_id", "\"SDOC-", "\"ZZZZ-"),
            ("invalid_hex", "7b7d", "7B7D"),
            ("missing_field", "\"lease_epoch\":3,", ""),
            ("unknown_variant", "\"commit\":", "\"purge\":"),
            ("invalid_type", "\"correlation_id\":7", "\"correlation_id\":\"7\""),
        ];
        let mut codes = BTreeSet::new();
        for (expected, from, to) in cases {
            assert!(text.contains(from), "mutation anchor {from}");
            let mutated = text.replacen(from, to, 1);
            assert_eq!(code(mutated.as_bytes()), expected, "mutation {from} -> {to}");
            codes.insert(expected);
        }
        assert_eq!(code(&[good.as_slice(), b"x"].concat()), "trailing_bytes");
        assert_eq!(code(&[good.as_slice(), b" "].concat()), "trailing_bytes");
        assert_eq!(code(b""), "empty");
        assert_eq!(code(&good[..good.len() - 1]), "syntax");
        assert_eq!(decode_envelope(&good, &tight).unwrap_err().code(), "oversize");
        assert_eq!(encode_envelope(&envelope, &tight).unwrap_err().code(), "oversize");
        codes.extend(["trailing_bytes", "empty", "syntax", "oversize"]);
        assert_eq!(codes.len(), 12, "every cause has its own stable code");
        // The encoder never produces what the decoder refuses.
        let mut foreign = envelope.clone();
        foreign.bridge_version = 2;
        assert_eq!(encode_envelope(&foreign, &limits).unwrap_err().code(), "wrong_version");
        // Replies are held to the same strictness.
        let reply = encode_reply(&Reply::new(7, ReplyBody::Ack), &limits).unwrap();
        assert_eq!(reply, br#"{"bridge_version":1,"correlation_id":7,"result":"ack"}"#);
        let extra = String::from_utf8(reply).unwrap().replacen("\"ack\"", "\"ack\",\"x\":1", 1);
        assert_eq!(decode_reply(extra.as_bytes(), &limits).unwrap_err().code(), "unknown_field");

        // The descriptor is JSON and lists every not-implemented host consumer as such.
        let descriptor: Value = serde_json::from_str(DESCRIPTOR).unwrap();
        assert!(descriptor["storage"].as_str().unwrap().starts_with("NONE"));
        let later = descriptor["later_host_adapters"].as_array().unwrap();
        assert!(later.len() >= 6);
        assert!(later.iter().all(|a| a["status"] == "NOT_IMPLEMENTED_HOST_ONLY"));
    });
}

// ----- review R5 regressions -------------------------------------------------------------------

const DOC_2: &str = "SDOC-019abcde-0000-7000-8000-000000000002";

fn other_actor(account: &str, principal: &str) -> ActorContext {
    ActorContext::new(account, principal, "owner", "owner-principal", "space", "session-2").unwrap()
}

fn request(
    actor: &ActorContext,
    resource: &str,
    key: Option<&str>,
    expected_revision: u64,
    lease_epoch: u64,
    body: Body,
) -> Vec<u8> {
    encode_envelope(
        &Envelope {
            bridge_version: BRIDGE_VERSION,
            correlation_id: 99,
            idempotency_key: key.map(str::to_owned),
            actor: ActorWire::from_context(actor),
            resource_id: resource.to_owned(),
            expected_revision,
            lease_epoch,
            body,
        },
        &Limits::default(),
    )
    .unwrap()
}

fn ask(host: &impl HostBridge, bytes: &[u8]) -> ReplyBody {
    decode_reply(&host.handle(bytes), &Limits::default()).unwrap().result
}

fn admit(host: &impl HostBridge, actor: &ActorContext, resource: &str, jobs: u32, bytes: u64) -> JobDecision {
    let body = Body::JobAdmit { action: ActionWire::Edit, jobs, bytes };
    match ask(host, &request(actor, resource, None, 0, EPOCH, body)) {
        ReplyBody::Job(decision) => decision,
        other => panic!("admit answered {other:?}"),
    }
}

fn release(host: &impl HostBridge, actor: &ActorContext, resource: &str, ticket: u64) -> ReplyBody {
    ask(host, &request(actor, resource, None, 0, EPOCH, Body::JobRelease { ticket }))
}

const NOT_OWNER: ReplyBody = ReplyBody::Job(JobDecision::Denied { code: JobDenyCode::NotTicketOwner });

#[test]
fn job_admission_needs_a_standing_grant() {
    bounded_case("job_admission_grant", || {
        let host = TransientHost::default();
        let (a, b) = (actor(), other_actor("account", "other-principal"));
        let stranger = other_actor("account-2", "principal");
        let doc = DomainId::parse(DOC).unwrap();
        host.grant(&a, &doc, Action::Edit);
        host.grant(&b, &doc, Action::Edit);

        // No grant, a grant for another action, and a withdrawn grant each get a typed denial
        // and no ticket; the holder of a standing grant is admitted.
        assert_eq!(
            admit(&host, &stranger, DOC, 1, 1),
            JobDecision::Denied { code: JobDenyCode::GrantMissing }
        );
        let export = Body::JobAdmit { action: ActionWire::Export, jobs: 1, bytes: 1 };
        assert_eq!(
            ask(&host, &request(&a, DOC, None, 0, EPOCH, export)),
            ReplyBody::Job(JobDecision::Denied { code: JobDenyCode::GrantMissing })
        );
        host.revoke(&b, &doc, Action::Edit);
        assert_eq!(
            admit(&host, &b, DOC, 1, 1),
            JobDecision::Denied { code: JobDenyCode::GrantRevoked }
        );
        assert_eq!(host.jobs_in_flight(), (0, 0), "denied admissions hold nothing");
        assert!(matches!(admit(&host, &a, DOC, 1, 1), JobDecision::Admitted { .. }));
    });
}

#[test]
fn job_tickets_are_released_only_by_their_owner() {
    bounded_case("job_ticket_owner", || {
        let host = TransientHost::default();
        let (a, b) = (actor(), other_actor("account", "other-principal"));
        let other_account = other_actor("account-2", "principal");
        let (doc, doc_2) = (DomainId::parse(DOC).unwrap(), DomainId::parse(DOC_2).unwrap());
        for who in [&a, &b, &other_account] {
            host.grant(who, &doc, Action::Edit);
        }
        host.grant(&a, &doc_2, Action::Edit);

        let JobDecision::Admitted { ticket } = admit(&host, &a, DOC, 1, 100) else {
            panic!("owner admitted");
        };
        assert_eq!(host.jobs_in_flight(), (1, 100));

        // Another principal of the same account (granted!), another account, and the owner on
        // another resource: each is refused with the typed error and nothing is released.
        for (who, resource) in [(&b, DOC), (&other_account, DOC), (&a, DOC_2)] {
            assert_eq!(release(&host, who, resource, ticket), NOT_OWNER);
            assert_eq!(host.jobs_in_flight(), (1, 100), "a foreign release freed nothing");
        }

        // The owner releases it; a repeated release is harmless.
        assert_eq!(release(&host, &a, DOC, ticket), ReplyBody::Ack);
        assert_eq!(host.jobs_in_flight(), (0, 0));
        assert_eq!(release(&host, &a, DOC, ticket), ReplyBody::Ack);

        // The accounting attack the finding describes: a full host stays full for a second
        // actor, whatever it tries to release.
        let JobDecision::Admitted { ticket: full } = admit(&host, &a, DOC, 4, 1) else {
            panic!("owner fills the capacity");
        };
        let refused = JobDecision::Refused { retry_after_ms: HostConfig::default().retry_after_ms };
        assert_eq!(admit(&host, &b, DOC, 1, 1), refused);
        assert_eq!(release(&host, &b, DOC, full), NOT_OWNER);
        assert_eq!(admit(&host, &b, DOC, 1, 1), refused, "the owner's slots were not freed");
        assert_eq!(release(&host, &a, DOC, full), ReplyBody::Ack);
        assert!(matches!(admit(&host, &b, DOC, 1, 1), JobDecision::Admitted { .. }));
    });
}

#[test]
fn lease_epoch_is_issued_by_the_host_not_adopted_from_callers() {
    bounded_case("lease_epoch_host_owned", || {
        let mut rig = rig(true);
        let mallory = other_actor("account", "mallory");
        let resource = rig.resource.clone();
        rig.host.inner.grant(&mallory, &resource, Action::Edit);
        let ReplyBody::Grant(grant) = ask(
            &*rig.host,
            &request(&mallory, DOC, None, 0, EPOCH, Body::GrantCheck { action: ActionWire::Edit }),
        ) else {
            panic!("grant answer");
        };
        assert!(grant.granted);
        let commit = |epoch: u64, key: &str| {
            let body = Body::Commit(CommitMsg {
                grant_token: grant.token,
                successor_revision: START + 1,
                binding: "0".repeat(64),
                document_hex: hex_encode(b"mallory"),
                revision_updates: Vec::new(),
            });
            ask(&*rig.host, &request(&mallory, DOC, Some(key), START, epoch, body))
        };

        // A granted editor claiming an epoch the host never issued is refused, whatever the
        // value, and nothing is applied or adopted.
        for (epoch, key) in [(u64::MAX, "mal-1"), (EPOCH + 1, "mal-2")] {
            assert_eq!(
                commit(epoch, key),
                ReplyBody::Commit(CommitDecision::Rejected { code: RejectCode::EpochNotIssued })
            );
            assert_eq!(rig.host.inner.lease_epoch(&resource), Some(EPOCH), "epoch {epoch} not adopted");
            assert_eq!(rig.host.inner.committed_revision(&resource), Some(START));
            assert_eq!(rig.host.inner.receipts().len(), 0);
        }

        // The legitimate writer, holding the host-issued epoch, is not fenced out.
        let applied = submit(&mut rig, &key("k-1"), &token()).unwrap();
        assert_eq!(applied.revision, START + 1);
        assert_eq!(rig.host.inner.lease_epoch(&resource), Some(EPOCH), "a commit leaves the epoch alone");
        assert_eq!(host_document(&rig), (START + 1, TO.to_owned()));
        assert_eq!(rig.host.inner.receipts().len(), 1);
    });
}

#[test]
fn hostile_sizes_are_refused_typed() {
    bounded_case("hostile_sizes", || {
        // Job admission: extreme costs and sums refuse (checked arithmetic), nothing is held.
        let host = TransientHost::default();
        let a = actor();
        let doc = DomainId::parse(DOC).unwrap();
        host.grant(&a, &doc, Action::Edit);
        let refused = JobDecision::Refused { retry_after_ms: HostConfig::default().retry_after_ms };
        assert_eq!(admit(&host, &a, DOC, u32::MAX, 1), refused, "dims u32::MAX");
        assert_eq!(admit(&host, &a, DOC, 1, u64::MAX), refused, "bytes u64::MAX");
        assert_eq!(host.jobs_in_flight(), (0, 0));
        assert!(matches!(admit(&host, &a, DOC, 3, 1), JobDecision::Admitted { .. }));
        assert_eq!(admit(&host, &a, DOC, u32::MAX, 0), refused, "sum overflows u32");
        assert_eq!(host.jobs_in_flight(), (3, 1));
        // Running aggregate over the byte budget (64 MiB default) is refused, not clipped.
        let forty = 40 * 1024 * 1024;
        assert!(matches!(admit(&host, &a, DOC, 0, forty), JobDecision::Admitted { .. }));
        assert_eq!(admit(&host, &a, DOC, 0, forty), refused, "aggregate over budget");
        assert_eq!(host.jobs_in_flight(), (3, forty + 1));

        // Wire: the document bound and the update-count bound are typed refusals, on both sides.
        let limits = Limits::default();
        let envelope = |hex_bytes: &[u8], updates: usize| Envelope {
            bridge_version: BRIDGE_VERSION,
            correlation_id: u64::MAX,
            idempotency_key: Some("k-1".into()),
            actor: ActorWire::from_context(&actor()),
            resource_id: DOC.into(),
            expected_revision: u64::MAX,
            lease_epoch: u64::MAX,
            body: Body::Commit(CommitMsg {
                grant_token: u64::MAX,
                successor_revision: u64::MAX,
                binding: "b".repeat(64),
                document_hex: hex_encode(hex_bytes),
                revision_updates: (0..updates)
                    .map(|_| RevisionUpdateWire { address: "{}".into(), previous_revision: 0, revision: u64::MAX })
                    .collect(),
            }),
        };
        let at_limit = envelope(b"12345678", MAX_REVISION_UPDATES);
        let bytes = encode_envelope(&at_limit, &limits).unwrap();
        assert_eq!(decode_envelope(&bytes, &limits).unwrap(), at_limit, "u64::MAX fields survive exactly");
        let tight = Limits { max_document_bytes: 4, ..limits };
        assert_eq!(encode_envelope(&at_limit, &tight).unwrap_err().code(), "document_too_large");
        assert_eq!(decode_envelope(&bytes, &tight).unwrap_err().code(), "document_too_large");
        let too_many = envelope(b"", MAX_REVISION_UPDATES + 1);
        assert_eq!(encode_envelope(&too_many, &limits).unwrap_err().code(), "too_many_updates");

        // Host: a successor that would wrap the revision counter is a typed rejection.
        let host = TransientHost::default();
        host.grant(&a, &doc, Action::Edit);
        host.seed_resource(&doc, u64::MAX, EPOCH, b"x");
        let ReplyBody::Grant(grant) = ask(
            &host,
            &request(&a, DOC, None, 0, EPOCH, Body::GrantCheck { action: ActionWire::Edit }),
        ) else {
            panic!("grant answer");
        };
        let body = Body::Commit(CommitMsg {
            grant_token: grant.token,
            successor_revision: u64::MAX,
            binding: "0".repeat(64),
            document_hex: hex_encode(b"y"),
            revision_updates: Vec::new(),
        });
        assert_eq!(
            ask(&host, &request(&a, DOC, Some("wrap-1"), u64::MAX, EPOCH, body)),
            ReplyBody::Commit(CommitDecision::Rejected { code: RejectCode::SuccessorMismatch })
        );
        assert_eq!(host.committed_revision(&doc), Some(u64::MAX));
        assert_eq!((host.receipts().len(), host.document_bytes(&doc).unwrap()), (0, b"x".to_vec()));
    });
}

#[test]
fn only_canonical_bytes_decode() {
    bounded_case("canonical_only", || {
        let limits = Limits::default();
        let envelope = Envelope {
            bridge_version: BRIDGE_VERSION,
            correlation_id: 7,
            idempotency_key: Some("k-1".into()),
            actor: ActorWire::from_context(&actor()),
            resource_id: DOC.into(),
            expected_revision: START,
            lease_epoch: EPOCH,
            body: Body::Commit(CommitMsg {
                grant_token: 1,
                successor_revision: START + 1,
                binding: "b".repeat(64),
                document_hex: hex_encode(b"{}"),
                revision_updates: Vec::new(),
            }),
        };
        let good = String::from_utf8(encode_envelope(&envelope, &limits).unwrap()).unwrap();
        assert_eq!(decode_envelope(good.as_bytes(), &limits).unwrap(), envelope);

        let code = |text: &str| decode_envelope(text.as_bytes(), &limits).unwrap_err().code();
        let without_version = good.replacen("\"bridge_version\":1,", "", 1);
        let reordered = format!("{},\"bridge_version\":1}}", without_version.strip_suffix('}').unwrap());
        let variants = [
            ("space after a colon", good.replacen("\"correlation_id\":7", "\"correlation_id\": 7", 1)),
            ("space after a comma", good.replacen("\"correlation_id\":7,", "\"correlation_id\":7, ", 1)),
            ("newline between fields", good.replacen(",\"actor\"", ",\n\"actor\"", 1)),
            ("tab inside the body", good.replacen("\"body\":{", "\"body\":{\t", 1)),
            ("key order", reordered),
            ("unicode escape", good.replacen("\"k-1\"", "\"k\\u002d1\"", 1)),
            ("solidus escape", good.replacen("\"k-1\"", "\"k\\/1\"", 1)),
        ];
        for (name, text) in &variants {
            assert_ne!(text, &good, "variant {name} changed the input");
            assert_eq!(code(text), "non_canonical", "{name}");
        }
        // An omitted null field decodes in serde but is not what the encoder emits.
        let grant = String::from_utf8(
            encode_envelope(
                &Envelope {
                    idempotency_key: None,
                    body: Body::GrantCheck { action: ActionWire::Edit },
                    ..envelope.clone()
                },
                &limits,
            )
            .unwrap(),
        )
        .unwrap();
        let omitted = grant.replacen("\"idempotency_key\":null,", "", 1);
        assert_ne!(omitted, grant);
        assert_eq!(code(&omitted), "non_canonical", "omitted null");
        assert!(decode_envelope(grant.as_bytes(), &limits).is_ok());

        // Comment and case decoys never reach the interpreter: the parser or the schema refuses them.
        let decoys = [
            ("block comment", good.replacen(",\"actor\"", ",/*x*/\"actor\"", 1)),
            ("line comment", format!("{good}//x")),
            ("upper-case key", good.replacen("\"correlation_id\"", "\"Correlation_Id\"", 1)),
            ("upper-case variant", good.replacen("\"commit\":", "\"Commit\":", 1)),
            ("single quotes", good.replacen("\"actor\"", "'actor'", 1)),
        ];
        for (name, text) in &decoys {
            assert_ne!(text, &good, "decoy {name} changed the input");
            assert!(decode_envelope(text.as_bytes(), &limits).is_err(), "decoy {name}");
        }

        // Number forms the encoder never emits are refused by the parser or the canonical check.
        for number in ["7.0", "7e0", "7E+0", "07", "+7", "0x7", "7.", "-0"] {
            let text = good.replacen("\"correlation_id\":7", &format!("\"correlation_id\":{number}"), 1);
            assert!(decode_envelope(text.as_bytes(), &limits).is_err(), "number form {number}");
        }

        // Replies are held to the same rule, and so is the host's boundary: a non-canonical
        // request earns a typed `non_canonical` error reply, not an interpretation.
        let ack = String::from_utf8(encode_reply(&Reply::new(7, ReplyBody::Ack), &limits).unwrap()).unwrap();
        let spaced = ack.replacen("\"result\":", "\"result\": ", 1);
        assert_eq!(decode_reply(spaced.as_bytes(), &limits).unwrap_err().code(), "non_canonical");
        let host = TransientHost::default();
        assert_eq!(
            ask(&host, variants[0].1.as_bytes()),
            ReplyBody::Error(ErrorReply { code: "non_canonical".into() })
        );
        assert_eq!(host.requests_seen(), 1);
        assert_eq!(host.receipts().len(), 0);
    });
}
