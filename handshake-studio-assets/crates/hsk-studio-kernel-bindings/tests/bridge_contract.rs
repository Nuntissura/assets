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
