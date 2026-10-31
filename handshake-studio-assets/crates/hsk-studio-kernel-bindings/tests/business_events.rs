//! Lane SMB-BUSINESS-EVENTS-1 (RF-07, MT-36230 asset side): the canonical business-event
//! envelope built from accord records, and the asset-local sink (STU-CON-003, STU-CON-013,
//! STU-CON-014). The catalog itself is accord's (pinned in accord tests/business_event_catalog.rs).
use hsk_studio_kernel_bindings::{
    BusinessEventDeliveryError, BusinessEventRecord, BusinessEventSink, MAX_BUSINESS_EVENT_ARTIFACTS,
    MAX_COMMAND_ID_BYTES, MAX_EVENT_BYTES, NOT_PRODUCED_ASSET_LOCALLY, StudioBusinessEvent, StudioBusinessEventId,
    TransientBusinessEvents, TransientEventStats, decode_business_event, encode_business_event,
};
use hsk_studio_accord::{ActorContext, CausationId, ClientSessionId, CommandContext, CommandId, CorrelationId};
use serde_json::Value;
use std::collections::BTreeSet;

const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";
const LAYER: &str = "SLYR-019abcde-0000-7000-8000-0000000000a1";
const DIGEST: &str = "3a7bd3e2360a3d29eea436fcfb7e44c735d117c42d1c1835420b6b9942dd4f1b";

fn actor() -> ActorContext {
    ActorContext::new("account-7", "principal-7", "owner", "owner-principal", "space", "session-7").unwrap()
}

/// A command context as SMB-ATTRIBUTION-1's Run mints it.
fn context(actor: ActorContext, session: &str, command: Option<&str>, correlation: u64, causation: Option<u64>) -> CommandContext {
    CommandContext::new(
        actor,
        ClientSessionId::parse(session).unwrap(),
        command.map(|c| CommandId::parse(c).unwrap()),
        3,
        CorrelationId::new(correlation),
        causation.map(CausationId::new),
    )
}

fn record(event: StudioBusinessEventId, revision: u64, sequence: u64) -> BusinessEventRecord {
    BusinessEventRecord::new(event, DOC, revision, sequence, context(actor(), "client-1", Some("layer.rename"), 1001, Some(1000))).unwrap()
}

fn event(event: StudioBusinessEventId, revision: u64) -> StudioBusinessEvent {
    StudioBusinessEvent::from_record(&record(event, revision, 0)).unwrap()
}

fn text(event: &StudioBusinessEvent) -> String {
    String::from_utf8(encode_business_event(event).unwrap()).unwrap()
}

#[test]
fn variants_without_a_producer_are_listed_not_faked() {
    let listed: Vec<&str> = NOT_PRODUCED_ASSET_LOCALLY.iter().map(|(v, _)| v.as_str()).collect();
    assert_eq!(
        listed,
        [
            "STUDIO_DOCUMENT_PROMOTED",
            "STUDIO_EDIT_PROPOSAL_RECORDED",
            "STUDIO_EDIT_PROMOTED",
            "STUDIO_EDIT_REJECTED",
            "STUDIO_SELECTION_CHANGED",
            "STUDIO_MASK_APPLIED",
            "STUDIO_ADJUSTMENT_APPLIED",
            "STUDIO_EFFECT_APPLIED",
            "STUDIO_STYLE_APPLIED",
            "STUDIO_COMPONENT_PUBLISHED",
            "STUDIO_VARIABLE_SET",
            "STUDIO_PROTOTYPE_EDITED",
            "STUDIO_EXPORT_RENDERED",
        ]
    );
    let reasons: BTreeSet<&str> = NOT_PRODUCED_ASSET_LOCALLY.iter().map(|(_, r)| r.code()).collect();
    assert_eq!(
        reasons,
        BTreeSet::from(["no_promotion_or_proposal_flow", "view_state_not_a_commit", "no_mapped_command", "export_outside_run"])
    );
    // Not produced does not mean untyped: each still builds and round-trips as an envelope.
    for (n, (variant, _)) in NOT_PRODUCED_ASSET_LOCALLY.iter().enumerate() {
        let e = event(*variant, u64::try_from(n).unwrap());
        assert_eq!(decode_business_event(&encode_business_event(&e).unwrap()).unwrap(), e);
    }
    // This binding does not re-declare the catalog: no source file spells a wire variant literal.
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("\"STUDIO_LAYER_MUTATED\""), "{} re-declares a business event", path.display());
    }
}

#[test]
fn envelopes_are_strict_and_refuse_typed() {
    // Round trip from an accord record, with digests and artifact refs.
    let r = record(StudioBusinessEventId::LayerMutated, 4, 1).with_artifacts(&[LAYER]).unwrap();
    let e = StudioBusinessEvent::from_record(&r).unwrap().with_digests(Some(DIGEST), None).unwrap();
    let bytes = encode_business_event(&e).unwrap();
    assert_eq!(decode_business_event(&bytes).unwrap(), e);
    let s = String::from_utf8(bytes).unwrap();
    assert!(s.starts_with(r#"{"schema_id":"hsk.studio.business_event@1","variant":"STUDIO_LAYER_MUTATED","event_family":"studio.document","business_id":"FR-EVT-STUDIO-LAYER-MUTATED","document":"SDOC-"#));
    assert_eq!(e.event_id(), format!("{DOC}@4#1"));
    assert_eq!(e.event_id(), r.event_id());
    assert_eq!(
        (e.correlation().value(), e.causation().map(CausationId::value), e.revision(), e.sequence(), e.client_session().as_str(), e.command_id().as_str()),
        (1001, Some(1000), 4, 1, "client-1", "layer.rename")
    );
    assert_eq!((e.actor(), e.artifacts()[0].as_str()), (&actor(), LAYER));

    let refused = |text: &str| decode_business_event(text.as_bytes()).unwrap_err().code();
    assert_eq!(refused(&s.replacen(r#""sequence""#, r#""x":1,"sequence""#, 1)), "unknown_field");
    assert_eq!(refused(&s.replacen(r#""sequence":1"#, r#""sequence":1,"sequence":1"#, 1)), "duplicate_field");
    assert_eq!(refused(&s.replacen("business_event@1", "business_event@2", 1)), "wrong_schema");
    assert_eq!(refused(&s.replacen("STUDIO_LAYER_MUTATED", "STUDIO_LAYER_RENAMED", 1)), "unknown_variant");
    assert_eq!(refused(&s.replacen(r#""studio.document""#, r#""studio.vector""#, 1)), "family_mismatch");
    assert_eq!(refused(&s.replacen("FR-EVT-STUDIO-LAYER-MUTATED", "FR-EVT-STUDIO-LAYER-CREATED", 1)), "business_id_mismatch");
    // A non-SDOC document: a valid layer id is still not a document.
    assert_eq!(refused(&s.replace(DOC, "SLYR-019abcde-0000-7000-8000-000000000001")), "invalid_document");
    // The event id is derived, never free: another sequence's or revision's id is refused.
    assert_eq!(refused(&s.replacen(&format!("{DOC}@4#1"), &format!("{DOC}@4#2"), 1)), "event_id_mismatch");
    assert_eq!(refused(&s.replacen(&format!("{DOC}@4#1"), &format!("{DOC}@5#1"), 1)), "event_id_mismatch");
    // Canonical only: whitespace, trailing bytes, an omitted null, a missing field.
    assert_eq!(refused(&s.replacen(",", ", ", 1)), "non_canonical");
    assert_eq!(refused(&format!("{s} ")), "trailing_bytes");
    assert_eq!(refused(&s.replacen(r#","output_digest":null"#, "", 1)), "non_canonical");
    assert_eq!(refused(&s.replacen(r#""sequence":1,"#, "", 1)), "missing_field");
    assert_eq!(refused(""), "empty");
    // Oversize is refused before parsing, and the largest valid event still fits.
    assert_eq!(refused(&format!("{{{}}}", " ".repeat(MAX_EVENT_BYTES))), "oversize");
    let long = "a".repeat(128);
    let big_actor = ActorContext::new(&long, &long, &long, &long, &long, &long).unwrap();
    let big = context(big_actor, &long, Some(&"a".repeat(MAX_COMMAND_ID_BYTES)), u64::MAX, Some(u64::MAX));
    let refs: Vec<String> = (0..MAX_BUSINESS_EVENT_ARTIFACTS).map(|i| format!("SLYR-019abcde-0000-7000-8000-{i:012x}")).collect();
    let refs: Vec<&str> = refs.iter().map(String::as_str).collect();
    let largest_record = BusinessEventRecord::new(StudioBusinessEventId::EditProposalRecorded, DOC, u64::MAX, u64::MAX, big)
        .unwrap()
        .with_artifacts(&refs)
        .unwrap();
    let largest = StudioBusinessEvent::from_record(&largest_record).unwrap().with_digests(Some(DIGEST), Some(DIGEST)).unwrap();
    assert_eq!(decode_business_event(&encode_business_event(&largest).unwrap()).unwrap(), largest);
    // Digests are lowercase SHA-256 hex.
    assert_eq!(event(StudioBusinessEventId::LayerMutated, 1).with_digests(Some(&DIGEST.to_uppercase()), None).unwrap_err().code(), "invalid_digest");
}

#[test]
fn envelopes_carry_ids_and_digests_only() {
    const DECOY: &str = "Quarterly Secret Poster";
    let e = event(StudioBusinessEventId::TextEdited, 9);
    let v: Value = serde_json::from_str(&text(&e)).unwrap();
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    let mut expected = vec![
        "schema_id", "variant", "event_family", "business_id", "document", "actor", "client_session", "event_id",
        "revision", "sequence", "correlation", "causation", "command_id", "input_digest", "output_digest", "artifacts",
    ];
    keys.sort_unstable();
    expected.sort_unstable();
    assert_eq!(keys, expected, "no name, label, text, path or font field exists");
    // accord's CommandId admits any bounded text; the envelope admits only the RUN_COMMANDS grammar.
    for command in [DECOY, "studio/../etc", "work/poster.psd", "Studio.Rename"] {
        let r = BusinessEventRecord::new(StudioBusinessEventId::TextEdited, DOC, 2, 0, context(actor(), "client-1", Some(command), 1, None)).unwrap();
        assert_eq!(StudioBusinessEvent::from_record(&r).unwrap_err().code(), "invalid_command_id");
    }
    assert_eq!(e.clone().with_digests(None, Some(DECOY)).unwrap_err().code(), "invalid_digest");
    // Lead decision 2026-10-31: the command id is required; a context without one is a typed
    // refusal, never an event with a null or invented id.
    let no_command = BusinessEventRecord::new(StudioBusinessEventId::LayerMutated, DOC, 2, 0, context(actor(), "client-1", None, 1, None)).unwrap();
    assert_eq!(StudioBusinessEvent::from_record(&no_command).unwrap_err().code(), "missing_command_id");
    let null = text(&e).replacen(r#""command_id":"layer.rename""#, r#""command_id":null"#, 1);
    assert_eq!(decode_business_event(null.as_bytes()).unwrap_err().code(), "invalid_type");
    // A control character in the client session or the command id on the wire is refused typed;
    // a decoy text artifact ref too.
    let control = text(&e).replacen(r#""client_session":"client-1""#, r#""client_session":"client\n1""#, 1);
    assert_eq!(decode_business_event(control.as_bytes()).unwrap_err().code(), "invalid_client_session");
    let control = text(&e).replacen(r#""command_id":"layer.rename""#, r#""command_id":"layer\trename""#, 1);
    assert_eq!(decode_business_event(control.as_bytes()).unwrap_err().code(), "invalid_command_id");
    let decoy_ref = text(&e).replacen(r#""artifacts":[]"#, &format!(r#""artifacts":["{DECOY}"]"#), 1);
    assert_eq!(decode_business_event(decoy_ref.as_bytes()).unwrap_err().code(), "invalid_artifact");
    assert!(!text(&e).contains(DECOY));
}

#[test]
fn transient_sink_is_bounded_idempotent_and_refuses_collisions() {
    // The sink is an accord BusinessEventSink: it takes records, not envelopes.
    let mut sink = TransientBusinessEvents::new(2);
    let as_sink: &mut dyn BusinessEventSink = &mut sink;
    let first = record(StudioBusinessEventId::LayerMutated, 4, 0);
    assert_eq!(as_sink.try_emit(&first), Ok(()));
    // An identical re-delivery is a replay, stored once.
    assert_eq!(as_sink.try_emit(&first), Ok(()));
    // The same id with a different envelope is an immutable-envelope collision.
    let colliding = record(StudioBusinessEventId::LayerCreated, 4, 0);
    assert_eq!(colliding.event_id(), first.event_id());
    assert_eq!(as_sink.try_emit(&colliding), Err(BusinessEventDeliveryError::Rejected));
    // A record without a command id makes no envelope: rejected, counted, not stored.
    let no_command = BusinessEventRecord::new(StudioBusinessEventId::LayerMutated, DOC, 5, 0, context(actor(), "client-1", None, 7, None)).unwrap();
    assert_eq!(as_sink.try_emit(&no_command), Err(BusinessEventDeliveryError::Rejected));
    assert_eq!(as_sink.try_emit(&record(StudioBusinessEventId::LayerMutated, 5, 0)), Ok(()));
    // Full: the new event is refused typed, nothing is evicted.
    assert_eq!(as_sink.try_emit(&record(StudioBusinessEventId::LayerMutated, 6, 0)), Err(BusinessEventDeliveryError::Saturated));
    let stored: Vec<u64> = sink.events().iter().map(StudioBusinessEvent::revision).collect();
    assert_eq!(stored, [4, 5]);
    assert_eq!(sink.events()[0], StudioBusinessEvent::from_record(&first).unwrap());
    assert_eq!(sink.stats(), TransientEventStats { accepted: 2, replays: 1, saturated: 1, collisions: 1, invalid: 1 });
    // The DESCRIPTOR declares the sink asset-local with no storage.
    let descriptor: Value = serde_json::from_str(hsk_studio_kernel_bindings::DESCRIPTOR).unwrap();
    assert!(descriptor["business_events"]["asset_local_sink"].as_str().unwrap().starts_with("TransientBusinessEvents: storage NONE"));
}
