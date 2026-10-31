//! Lane SMB-BUSINESS-EVENTS-1 (RF-07, MT-36230 asset side): the business-event catalog, the
//! typed envelope and the asset-local sink (STU-CON-003, STU-CON-013, STU-CON-014).
use hsk_studio_accord::{ActorContext, CausationId, ClientSessionId, CommandContext, CommandId, CorrelationId};
use hsk_studio_kernel_bindings::{
    BusinessEventPort, EventDeliveryError, MAX_ARTIFACT_REFS, MAX_COMMAND_ID_BYTES,
    MAX_EVENT_BYTES, NOT_PRODUCED_ASSET_LOCALLY, StudioBusinessEvent, StudioEventVariant,
    TransientBusinessEvents, TransientEventStats, decode_business_event, encode_business_event,
};
use serde_json::Value;
use std::collections::BTreeSet;

const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";
const LAYER: &str = "SLYR-019abcde-0000-7000-8000-0000000000a1";
const DIGEST: &str = "3a7bd3e2360a3d29eea436fcfb7e44c735d117c42d1c1835420b6b9942dd4f1b";

/// STU-CON-014 at spec v02.225, copied verbatim from the lane brief's table.
const SPEC_TABLE: &str = "\
| `STUDIO_DOCUMENT_CREATED` | `studio.document` | `FR-EVT-STUDIO-DOCUMENT-CREATED` |
| `STUDIO_DOCUMENT_PROMOTED` | `studio.document` | `FR-EVT-STUDIO-DOCUMENT-PROMOTED` |
| `STUDIO_LAYER_CREATED` | `studio.document` | `FR-EVT-STUDIO-LAYER-CREATED` |
| `STUDIO_LAYER_MUTATED` | `studio.document` | `FR-EVT-STUDIO-LAYER-MUTATED` |
| `STUDIO_SELECTION_CHANGED` | `studio.document` | `FR-EVT-STUDIO-SELECTION-CHANGED` |
| `STUDIO_MASK_APPLIED` | `studio.raster` | `FR-EVT-STUDIO-MASK-APPLIED` |
| `STUDIO_ADJUSTMENT_APPLIED` | `studio.color` | `FR-EVT-STUDIO-ADJUSTMENT-APPLIED` |
| `STUDIO_EFFECT_APPLIED` | `studio.effect` | `FR-EVT-STUDIO-EFFECT-APPLIED` |
| `STUDIO_VECTOR_PATH_EDITED` | `studio.vector` | `FR-EVT-STUDIO-VECTOR-PATH-EDITED` |
| `STUDIO_TEXT_EDITED` | `studio.typography` | `FR-EVT-STUDIO-TEXT-EDITED` |
| `STUDIO_STYLE_APPLIED` | `studio.layout` | `FR-EVT-STUDIO-STYLE-APPLIED` |
| `STUDIO_COMPONENT_PUBLISHED` | `studio.design_system` | `FR-EVT-STUDIO-COMPONENT-PUBLISHED` |
| `STUDIO_VARIABLE_SET` | `studio.design_system` | `FR-EVT-STUDIO-VARIABLE-SET` |
| `STUDIO_PROTOTYPE_EDITED` | `studio.prototype` | `FR-EVT-STUDIO-PROTOTYPE-EDITED` |
| `STUDIO_RAW_DEVELOPED` | `studio.color` | `FR-EVT-STUDIO-RAW-DEVELOPED` |
| `STUDIO_EXPORT_RENDERED` | `studio.export` | `FR-EVT-STUDIO-EXPORT-RENDERED` |
| `STUDIO_IMPORT_COMPLETED` | `studio.document` | `FR-EVT-STUDIO-IMPORT-COMPLETED` |
| `STUDIO_EDIT_PROPOSAL_RECORDED` | `studio.proposal` | `FR-EVT-STUDIO-EDIT-PROPOSAL-RECORDED` |
| `STUDIO_EDIT_PROMOTED` | `studio.proposal` | `FR-EVT-STUDIO-EDIT-PROMOTED` |
| `STUDIO_EDIT_REJECTED` | `studio.proposal` | `FR-EVT-STUDIO-EDIT-REJECTED` |
| `STUDIO_HISTORY_UNDONE` | `studio.history` | `FR-EVT-STUDIO-HISTORY-UNDONE` |
| `STUDIO_HISTORY_REDONE` | `studio.history` | `FR-EVT-STUDIO-HISTORY-REDONE` |
";

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

fn event(variant: StudioEventVariant, sequence: u64) -> StudioBusinessEvent {
    let context = context(actor(), "client-1", Some("layer.rename"), 1001, Some(1000));
    StudioBusinessEvent::new(variant, DOC, &context, sequence, 4).unwrap()
}

fn text(event: &StudioBusinessEvent) -> String {
    String::from_utf8(encode_business_event(event).unwrap()).unwrap()
}

#[test]
fn business_event_catalog_equals_the_spec_table() {
    let rows: Vec<String> = StudioEventVariant::ALL
        .iter()
        .map(|v| format!("| `{}` | `{}` | `{}` |\n", v.as_str(), v.family(), v.business_id()))
        .collect();
    assert_eq!(rows.concat(), SPEC_TABLE);
    assert_eq!(StudioEventVariant::ALL.len(), 22);
    let wire: BTreeSet<&str> = StudioEventVariant::ALL.iter().map(|v| v.as_str()).collect();
    let ids: BTreeSet<&str> = StudioEventVariant::ALL.iter().map(|v| v.business_id()).collect();
    assert_eq!((wire.len(), ids.len()), (22, 22), "wire names and business ids are distinct");
    for v in StudioEventVariant::ALL {
        assert_eq!(StudioEventVariant::parse(v.as_str()), Some(v));
        assert_eq!(StudioEventVariant::parse(&v.as_str().to_lowercase()), None, "wire names are exact");
    }
    // STU-CON-014: selection, proposal and rejection never claim a document promotion.
    let no_promotion: Vec<StudioEventVariant> =
        StudioEventVariant::ALL.into_iter().filter(|v| !v.claims_document_promotion()).collect();
    assert_eq!(
        no_promotion,
        [StudioEventVariant::SelectionChanged, StudioEventVariant::EditProposalRecorded, StudioEventVariant::EditRejected]
    );
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
    // No source file of this crate other than business_event.rs spells a wire variant: the
    // binding owns the catalog, nothing else re-declares it.
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") || path.ends_with("business_event.rs") || path.ends_with("lib.rs") {
            continue;
        }
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("STUDIO_LAYER_MUTATED"), "{} re-declares a business event", path.display());
    }
}

#[test]
fn envelopes_are_strict_and_refuse_typed() {
    // Round trip, with digests and artifact refs.
    let e = event(StudioEventVariant::LayerMutated, 3)
        .with_digests(Some(DIGEST), None)
        .unwrap()
        .with_artifacts(&[LAYER])
        .unwrap();
    let bytes = encode_business_event(&e).unwrap();
    assert_eq!(decode_business_event(&bytes).unwrap(), e);
    let s = String::from_utf8(bytes).unwrap();
    assert!(s.starts_with(r#"{"schema_id":"hsk.studio.business_event@1","variant":"STUDIO_LAYER_MUTATED","event_family":"studio.document","business_id":"FR-EVT-STUDIO-LAYER-MUTATED","document":"SDOC-"#));
    assert_eq!(e.event_id(), format!("{DOC}#3"));
    assert_eq!(
        (e.correlation().value(), e.causation().map(CausationId::value), e.revision(), e.client_session().as_str(), e.command_id().as_str()),
        (1001, Some(1000), 4, "client-1", "layer.rename")
    );
    assert_eq!(e.actor(), &actor());

    let refused = |text: &str| decode_business_event(text.as_bytes()).unwrap_err().code();
    // Unknown field, duplicate field, wrong schema, family and business id that do not match.
    assert_eq!(refused(&s.replacen(r#""sequence""#, r#""x":1,"sequence""#, 1)), "unknown_field");
    assert_eq!(refused(&s.replacen(r#""sequence":3"#, r#""sequence":3,"sequence":3"#, 1)), "duplicate_field");
    assert_eq!(refused(&s.replacen("business_event@1", "business_event@2", 1)), "wrong_schema");
    assert_eq!(refused(&s.replacen("STUDIO_LAYER_MUTATED", "STUDIO_LAYER_RENAMED", 1)), "unknown_variant");
    assert_eq!(refused(&s.replacen(r#""studio.document""#, r#""studio.vector""#, 1)), "family_mismatch");
    assert_eq!(refused(&s.replacen("FR-EVT-STUDIO-LAYER-MUTATED", "FR-EVT-STUDIO-LAYER-CREATED", 1)), "business_id_mismatch");
    // A non-SDOC document: a valid layer id is still not a document.
    let slyr_doc = s.replace(DOC, "SLYR-019abcde-0000-7000-8000-000000000001");
    assert_eq!(refused(&slyr_doc), "invalid_document");
    let plain = context(actor(), "c", Some("x"), 1, None);
    assert_eq!(StudioBusinessEvent::new(StudioEventVariant::LayerMutated, LAYER, &plain, 0, 1).unwrap_err().code(), "invalid_document");
    // The event id is derived, never free: another sequence's id is refused.
    assert_eq!(refused(&s.replacen(&format!("{DOC}#3"), &format!("{DOC}#4"), 1)), "event_id_mismatch");
    // Canonical only: whitespace, trailing bytes, an omitted null.
    assert_eq!(refused(&s.replacen(",", ", ", 1)), "non_canonical");
    assert_eq!(refused(&format!("{s} ")), "trailing_bytes");
    assert_eq!(refused(&s.replacen(r#","output_digest":null"#, "", 1)), "non_canonical");
    assert_eq!(refused(&s.replacen(r#""sequence":3,"#, "", 1)), "missing_field");
    assert_eq!(refused(""), "empty");
    // Oversize is refused before parsing, and the largest valid event still fits.
    assert_eq!(refused(&format!("{{{}}}", " ".repeat(MAX_EVENT_BYTES))), "oversize");
    let long = "a".repeat(128);
    let big_actor = ActorContext::new(&long, &long, &long, &long, &long, &long).unwrap();
    let big = context(big_actor, &long, Some(&"a".repeat(MAX_COMMAND_ID_BYTES)), u64::MAX, Some(u64::MAX));
    let refs: Vec<String> =
        (0..MAX_ARTIFACT_REFS).map(|i| format!("SLYR-019abcde-0000-7000-8000-{i:012x}")).collect();
    let refs: Vec<&str> = refs.iter().map(String::as_str).collect();
    let largest = StudioBusinessEvent::new(StudioEventVariant::EditProposalRecorded, DOC, &big, u64::MAX, u64::MAX)
        .unwrap()
        .with_digests(Some(DIGEST), Some(DIGEST))
        .unwrap()
        .with_artifacts(&refs)
        .unwrap();
    assert_eq!(decode_business_event(&encode_business_event(&largest).unwrap()).unwrap(), largest);
    // Artifact refs are bounded before they are copied, and typed.
    let mut too_many = refs.clone();
    too_many.push(LAYER);
    assert_eq!(event(StudioEventVariant::LayerMutated, 1).with_artifacts(&too_many).unwrap_err().code(), "too_many_artifacts");
    assert_eq!(event(StudioEventVariant::LayerMutated, 1).with_artifacts(&[LAYER, LAYER]).unwrap_err().code(), "invalid_artifact");
    assert_eq!(event(StudioEventVariant::LayerMutated, 1).with_digests(Some(&DIGEST.to_uppercase()), None).unwrap_err().code(), "invalid_digest");
}

#[test]
fn envelopes_carry_ids_and_digests_only() {
    const DECOY: &str = "Quarterly Secret Poster";
    let e = event(StudioEventVariant::TextEdited, 9);
    let v: Value = serde_json::from_str(&text(&e)).unwrap();
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    let mut expected = vec![
        "schema_id", "variant", "event_family", "business_id", "document", "actor", "client_session", "event_id",
        "sequence", "correlation", "causation", "revision", "command_id", "input_digest", "output_digest", "artifacts",
    ];
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    expected.sort_unstable();
    assert_eq!(sorted, expected, "no name, label, text, path or font field exists");
    // The only free-form slots refuse text: command ids are [a-z0-9._-], artifacts are domain ids,
    // digests are hex.
    // accord's CommandId admits any bounded text; the envelope admits only the RUN_COMMANDS grammar.
    for command in [DECOY, "studio/../etc", "work/poster.psd", "Studio.Rename"] {
        let decoy = context(actor(), "client-1", Some(command), 1, None);
        assert_eq!(StudioBusinessEvent::new(StudioEventVariant::TextEdited, DOC, &decoy, 1, 2).unwrap_err().code(), "invalid_command_id");
    }
    assert_eq!(e.clone().with_artifacts(&[DECOY]).unwrap_err().code(), "invalid_artifact");
    assert_eq!(e.clone().with_digests(None, Some(DECOY)).unwrap_err().code(), "invalid_digest");
    // A control character in the client session or the command id on the wire is refused typed.
    let control = text(&e).replacen(r#""client_session":"client-1""#, r#""client_session":"client\n1""#, 1);
    assert_eq!(decode_business_event(control.as_bytes()).unwrap_err().code(), "invalid_client_session");
    let control = text(&e).replacen(r#""command_id":"layer.rename""#, r#""command_id":"layer\trename""#, 1);
    assert_eq!(decode_business_event(control.as_bytes()).unwrap_err().code(), "invalid_command_id");
    assert!(!text(&e).contains(DECOY));
    // Lead decision 2026-10-31: the command id is required; a missing one is a typed refusal,
    // never an event with a null or invented id.
    assert_eq!(
        StudioBusinessEvent::new(StudioEventVariant::LayerMutated, DOC, &context(actor(), "client-1", None, 1, None), 1, 2).unwrap_err().code(),
        "missing_command_id"
    );
    let v_null = text(&e).replacen(r#""command_id":"layer.rename""#, r#""command_id":null"#, 1);
    assert_eq!(decode_business_event(v_null.as_bytes()).unwrap_err().code(), "invalid_type");
}

/// A port that reports what an adapter reports when it cannot know whether bytes escaped.
struct UnknownOutcomePort;
impl BusinessEventPort for UnknownOutcomePort {
    fn try_emit(&self, _: &StudioBusinessEvent) -> Result<(), EventDeliveryError> {
        Err(EventDeliveryError::Indeterminate)
    }
}

#[test]
fn transient_sink_is_bounded_idempotent_and_refuses_collisions() {
    let sink = TransientBusinessEvents::new(2);
    let first = event(StudioEventVariant::LayerMutated, 1);
    assert_eq!(sink.try_emit(&first), Ok(()));
    // An identical re-delivery is a replay, stored once.
    assert_eq!(sink.try_emit(&first), Ok(()));
    // The same id with a different envelope is an immutable-envelope collision.
    let colliding = event(StudioEventVariant::LayerCreated, 1);
    assert_eq!(colliding.event_id(), first.event_id());
    assert_eq!(sink.try_emit(&colliding), Err(EventDeliveryError::Rejected));
    assert_eq!(sink.try_emit(&event(StudioEventVariant::LayerMutated, 2)), Ok(()));
    // Full: the new event is refused typed, nothing is evicted.
    assert_eq!(sink.try_emit(&event(StudioEventVariant::LayerMutated, 3)), Err(EventDeliveryError::Saturated));
    let stored: Vec<u64> = sink.events().iter().map(StudioBusinessEvent::sequence).collect();
    assert_eq!(stored, [1, 2]);
    assert_eq!(sink.events()[0], first);
    assert_eq!(sink.stats(), TransientEventStats { accepted: 2, replays: 1, saturated: 1, collisions: 1 });
    // Indeterminate is reported as such and is the only error that needs reconciliation.
    let port: &dyn BusinessEventPort = &UnknownOutcomePort;
    let error = port.try_emit(&first).unwrap_err();
    assert_eq!((error.code(), error.needs_reconciliation()), ("indeterminate", true));
    for other in [EventDeliveryError::Saturated, EventDeliveryError::Rejected, EventDeliveryError::Unavailable] {
        assert!(!other.needs_reconciliation(), "{other}");
    }
    // The DESCRIPTOR declares the sink asset-local with no storage.
    let descriptor: Value = serde_json::from_str(hsk_studio_kernel_bindings::DESCRIPTOR).unwrap();
    assert!(descriptor["business_events"]["asset_local_sink"].as_str().unwrap().starts_with("TransientBusinessEvents: storage NONE"));
}
