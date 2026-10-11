//! Runs one scenario of the bridge end to end (session -> envelopes -> transient host) and prints
//! the receipt chain as JSON. Memory only: nothing is read from or written to disk.
//!
//! kernel-bindings-consumer --descriptor | --roundtrip | --revoke | --drop-ack | --cancel
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio as folio;
use hsk_studio_kernel_bindings::*;
use hsk_studio_observe::{DeliveryClass, DeliveryError, SinkPort};
use hsk_studio_session::{
    Action, CHRONICLE_PROVIDER_ID, IdempotencyKey, Session, SessionConfig, SessionPorts,
};
use serde_json::{Value, json};
use std::sync::Arc;

const PACKAGE: &[u8] = include_bytes!("../tests/fixtures/doc-rev9.handshake");
const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";
const LAYER_1: &str = "SLYR-019abcde-0000-7000-8000-000000000001";
const EPOCH: u64 = 3;

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

struct Quiet;
impl SinkPort for Quiet {
    fn try_send(&mut self, _: DeliveryClass, _: &[u8]) -> Result<(), DeliveryError> {
        Ok(())
    }
}

/// Cancels the session's token as soon as the host has handled a commit.
struct CancelAfterCommit {
    inner: TransientHost,
    token: CancellationToken,
}
impl HostBridge for CancelAfterCommit {
    fn handle(&self, request: &[u8]) -> Vec<u8> {
        let reply = self.inner.handle(request);
        if let Ok(envelope) = decode_envelope(request, &Limits::default())
            && matches!(envelope.body, Body::Commit(_))
        {
            self.token.cancel();
        }
        reply
    }
}

fn patch() -> Vec<u8> {
    let address = json!({"target":{"node_kind":"layer","node_id":LAYER_1},"property":"name"});
    serde_json::to_vec(&json!({
        "transport_version":1,"document_id":DOC,"base_revision":9,"command_id":"bridge-demo-1","correlation_id":1,
        "actor":{"account_id":"account","principal_id":"principal","owner_account_id":"owner","owner_principal_id":"owner-principal","access_space_id":"space","session_id":"session"},
        "reads":[{"address":address,"expected_revision":0,"expected_value":{"value_kind":"string","value":"renamed-by-session"}}],
        "writes":[{"address":address,"value":{"value_kind":"string","value":"renamed-by-bridge"}}]
    }))
    .map_err(|e| e.to_string())
    .unwrap_or_default()
}

fn scenario(name: &str) -> Result<Value, String> {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session")
        .map_err(|e| e.to_string())?;
    let resource = DomainId::parse(DOC).map_err(|e| e.to_string())?;
    let token = CancellationToken::default();
    let host = Arc::new(CancelAfterCommit {
        inner: TransientHost::default(),
        token: token.clone(),
    });
    let bridge = BridgePorts::new(
        Arc::clone(&host),
        actor.clone(),
        resource.clone(),
        EPOCH,
        Limits::default(),
    )
    .map_err(|e| e.to_string())?;
    let mut config = SessionConfig::new(actor.clone());
    config.epoch = EPOCH;
    let ports = SessionPorts::new(Box::new(Quiet))
        .with_resolver(Box::new(Granted))
        .with_grants(bridge.grants())
        .with_commits(bridge.commits())
        .with_jobs(bridge.jobs());
    let mut session = Session::open_bytes(PACKAGE, config, ports, &CancellationToken::default())
        .map_err(|e| e.to_string())?;
    host.inner.seed_resource(&resource, session.document_revision(), EPOCH, session.snapshot().encoded_bytes());
    if name != "revoke" {
        host.inner.grant(&actor, &resource, Action::Edit);
    }
    if name == "drop-ack" {
        host.inner.plan_fault(Fault::DropAck);
    }
    // Only the cancel scenario hands the session the token the host trips after a commit.
    let live = if name == "cancel" { token } else { CancellationToken::default() };
    let key = IdempotencyKey::parse("demo-key-1").map_err(|e| e.to_string())?;
    let result = session.submit_keyed(&key, CHRONICLE_PROVIDER_ID, &patch(), 9, &live);
    let outcome = match &result {
        Ok(applied) => json!({"ok":true,"revision":applied.revision,"replayed":applied.replayed,"cancel_too_late":applied.cancel_too_late,"receipt":applied.commit.as_ref().map(|r| json!({"revision":r.revision,"sequence":r.sequence}))}),
        Err(failed) => json!({"ok":false,"code":failed.code()}),
    };
    let mut reconciled = Value::Null;
    if matches!(&result, Err(f) if f.code() == "outcome_unknown") {
        let report = session.reconcile(&CancellationToken::default()).map_err(|e| e.to_string())?;
        reconciled = json!({"adopted":report.adopted,"cleared":report.cleared,"still_unknown":report.still_unknown});
    }
    let kinds: Vec<String> = bridge
        .sent_requests()
        .iter()
        .filter_map(|bytes| decode_envelope(bytes, &Limits::default()).ok())
        .map(|e| match e.body {
            Body::GrantCheck { .. } => "grant_check",
            Body::Commit(_) => "commit",
            Body::Lookup => "lookup",
            Body::JobAdmit { .. } => "job_admit",
            Body::JobRelease { .. } => "job_release",
            Body::Diagnostic(_) => "diagnostic",
        }
        .to_owned())
        .collect();
    let diagnostics: Vec<Value> = host
        .inner
        .diagnostics()
        .iter()
        .map(|d| json!({"outcome":format!("{:?}", d.message.outcome),"code":d.message.code}))
        .collect();
    Ok(json!({
        "scenario": name,
        "storage": "memory only (transient host); nothing persisted",
        "session": outcome,
        "reconcile": reconciled,
        "session_revision": session.document_revision(),
        "host": {
            "committed_revision": host.inner.committed_revision(&resource),
            "receipts": host.inner.receipts().iter().map(|r| json!({"revision":r.revision,"sequence":r.sequence,"host_ref":r.host_ref})).collect::<Vec<_>>(),
            "idempotency_entries": host.inner.idempotency_len(),
            "diagnostics": diagnostics,
            "jobs_in_flight": host.inner.jobs_in_flight().0,
        },
        "requests_sent": kinds,
    }))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("--descriptor") => Ok(DESCRIPTOR.to_owned()),
        Some(flag @ ("--roundtrip" | "--revoke" | "--drop-ack" | "--cancel")) => {
            scenario(&flag[2..]).map(|value| value.to_string())
        }
        _ => Err("usage: --descriptor | --roundtrip | --revoke | --drop-ack | --cancel".to_owned()),
    };
    match result {
        Ok(text) => println!("{text}"),
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    }
}
