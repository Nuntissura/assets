//! Lane SMB-ATTRIBUTION-1 (RF-06): the envelope's optional typed command context. An envelope
//! without it keeps its exact bytes; with it, accord's `CommandContext` round-trips through the one
//! adapter; replies still match on the per-exchange `correlation_id`, never on the command
//! correlation; hostile client sessions and command ids are refused typed on both sides.
use hsk_studio_accord::{
    ActorContext, CausationId, ClientSessionId, CommandContext, CommandId, CorrelationId,
    DomainId, ValidationError,
};
use hsk_studio_kernel_bindings::*;
use hsk_studio_session::{Action, GrantError};
use std::sync::Arc;

const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";

fn actor(session: &str) -> ActorContext {
    ActorContext::new("account", "principal", "owner", "owner-principal", "space", session).unwrap()
}

fn context(actor: ActorContext, correlation: u64) -> CommandContext {
    CommandContext::new(
        actor,
        ClientSessionId::parse("window-1").unwrap(),
        Some(CommandId::parse("layer.rename").unwrap()),
        9,
        CorrelationId::new(correlation),
        Some(CausationId::new(correlation - 1)),
    )
}

fn grant_check(command: Option<CommandContextWire>) -> Envelope {
    Envelope {
        bridge_version: BRIDGE_VERSION,
        correlation_id: 7,
        idempotency_key: None,
        actor: ActorWire::from_context(&actor("session")),
        resource_id: DOC.into(),
        expected_revision: 9,
        lease_epoch: 3,
        body: Body::GrantCheck { action: ActionWire::Edit },
        command,
    }
}

#[test]
fn an_envelope_without_command_keeps_its_bytes() {
    // The pre-lane encoding of this envelope (struct field order, no whitespace): no `command`
    // key, not even `null`.
    let before = concat!(
        r#"{"bridge_version":1,"correlation_id":7,"idempotency_key":null,"#,
        r#""actor":{"account_id":"account","principal_id":"principal","owner_account_id":"owner","#,
        r#""owner_principal_id":"owner-principal","access_space_id":"space","session_id":"session"},"#,
        r#""resource_id":"SDOC-019abcde-0000-7000-8000-000000000001","expected_revision":9,"#,
        r#""lease_epoch":3,"body":{"grant_check":{"action":"edit"}}}"#
    );
    let limits = Limits::default();
    let bytes = encode_envelope(&grant_check(None), &limits).unwrap();
    assert_eq!(String::from_utf8(bytes.clone()).unwrap(), before);
    assert_eq!(decode_envelope(before.as_bytes(), &limits).unwrap(), grant_check(None));
    // An explicit null is not what the encoder emits.
    let null = before.replacen("}}}", "}},\"command\":null}", 1);
    assert_eq!(decode_envelope(null.as_bytes(), &limits).unwrap_err().code(), "non_canonical");
}

#[test]
fn a_command_context_round_trips_through_the_envelope() {
    let limits = Limits::default();
    let typed = context(actor("session"), 1000);
    let envelope = grant_check(Some(CommandContextWire::from_context(&typed)));
    let bytes = encode_envelope(&envelope, &limits).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(
        text.ends_with(r#","command":{"command_id":"layer.rename","client_session":"window-1","base_revision":9,"correlation":1000,"causation":999}}"#),
        "{text}"
    );
    let decoded = decode_envelope(&bytes, &limits).unwrap();
    assert_eq!(decoded, envelope);
    assert_eq!(decoded.correlation_id, 7, "the exchange counter is its own field");
    assert_eq!(decoded.command.as_ref().unwrap().to_context(&decoded.actor).unwrap(), typed);
    // A command without a registered id or a cause round-trips with nulls.
    let bare = CommandContext::new(actor("session"), ClientSessionId::parse("cli").unwrap(), None, 0, CorrelationId::new(u64::MAX), None);
    let envelope = grant_check(Some(CommandContextWire::from_context(&bare)));
    let decoded = decode_envelope(&encode_envelope(&envelope, &limits).unwrap(), &limits).unwrap();
    assert_eq!(decoded.command.unwrap().to_context(&decoded.actor).unwrap(), bare);
}

/// Answers with the transient host's reply, but echoing the command correlation instead of the
/// exchange id (the exchange id when the request carries no command).
struct EchoesCommandCorrelation(TransientHost);
impl HostBridge for EchoesCommandCorrelation {
    fn handle(&self, request: &[u8]) -> Vec<u8> {
        let limits = Limits::default();
        let reply = decode_reply(&self.0.handle(request), &limits).unwrap();
        let envelope = decode_envelope(request, &limits).unwrap();
        let echoed = envelope.command.map_or(envelope.correlation_id, |c| c.correlation);
        encode_reply(&Reply::new(echoed, reply.result), &limits).unwrap()
    }
}

#[test]
fn reply_matching_uses_the_exchange_id_not_the_command_correlation() {
    let resource = DomainId::parse(DOC).unwrap();
    let me = actor("session");
    let host = Arc::new(TransientHost::default());
    host.grant(&me, &resource, Action::Edit);
    let ports = BridgePorts::new(Arc::clone(&host), me.clone(), resource.clone(), 3, Limits::default()).unwrap();
    ports.set_command_context(Some(context(me.clone(), 1000)));
    assert!(ports.grants().check(&me, &resource, Action::Edit).is_ok(), "the echo of the exchange id matches");
    let sent = decode_envelope(ports.sent_requests().last().unwrap(), &Limits::default()).unwrap();
    assert_eq!((sent.correlation_id, sent.command.as_ref().map(|c| c.correlation)), (1, Some(1000)));

    // A reply quoting the command correlation is a reply to no exchange: refused (stale-reply rule).
    let echo = Arc::new(EchoesCommandCorrelation(TransientHost::default()));
    echo.0.grant(&me, &resource, Action::Edit);
    let ports = BridgePorts::new(Arc::clone(&echo), me.clone(), resource.clone(), 3, Limits::default()).unwrap();
    ports.set_command_context(Some(context(me.clone(), 1)));
    assert!(ports.grants().check(&me, &resource, Action::Edit).is_ok(), "1 == 1: the exchange id itself");
    ports.set_command_context(Some(context(me.clone(), 1000)));
    assert_eq!(ports.grants().check(&me, &resource, Action::Edit).unwrap_err(), GrantError::Missing);

    // A context for another actor is never attached to this request: nothing is sent.
    let before = ports.sent_count();
    let other = actor("other-session");
    assert_eq!(ports.grants().check(&other, &resource, Action::Edit).unwrap_err(), GrantError::Missing);
    assert_eq!(ports.sent_count(), before);
    ports.set_command_context(None);
    let _ = ports.grants().check(&other, &resource, Action::Edit);
    assert_eq!(ports.sent_count(), before + 1, "cleared: the request goes out without a command");
}

#[test]
fn hostile_client_sessions_and_command_ids_are_refused_typed() {
    let hostile = ["", &"x".repeat(129), "a\u{0}b", "a\u{1f}b", "a\u{7f}b", "a\u{85}b", &"\u{e9}".repeat(65)];
    for value in hostile {
        assert_eq!(ClientSessionId::parse(value).unwrap_err(), ValidationError::InvalidContext, "{value:?}");
        assert_eq!(CommandId::parse(value).unwrap_err(), ValidationError::InvalidContext, "{value:?}");
    }
    assert!(ClientSessionId::parse(&"x".repeat(128)).is_ok() && CommandId::parse(&"\u{e9}".repeat(64)).is_ok());

    let limits = Limits::default();
    let wire = |client_session: &str, command_id: Option<&str>| CommandContextWire {
        command_id: command_id.map(str::to_owned),
        client_session: client_session.to_owned(),
        base_revision: 0,
        correlation: 1,
        causation: None,
    };
    // Encode refuses what it could not decode.
    for (session, command) in [("", None), ("ok", Some("")), ("a\u{1}b", None), ("ok", Some("a\nb"))] {
        let code = encode_envelope(&grant_check(Some(wire(session, command))), &limits).unwrap_err().code();
        assert_eq!(code, "invalid_command_context", "{session:?} {command:?}");
    }
    // Decode: canonical bytes carrying a hostile value are refused after parsing, typed.
    let good = String::from_utf8(encode_envelope(&grant_check(Some(wire("cs-1", Some("cmd")))), &limits).unwrap()).unwrap();
    let long = format!("\"{}\"", "y".repeat(129));
    for (from, to) in [("\"cs-1\"", "\"\""), ("\"cs-1\"", "\"c\\u0001s\""), ("\"cmd\"", "\"\""), ("\"cmd\"", long.as_str())] {
        let text = good.replacen(from, to, 1);
        assert_ne!(text, good);
        assert_eq!(decode_envelope(text.as_bytes(), &limits).unwrap_err().code(), "invalid_command_context", "{to}");
    }
}
