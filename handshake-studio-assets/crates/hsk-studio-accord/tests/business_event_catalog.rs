//! Lane SMB-BUSINESS-EVENTS-1 (RF-07): the business-event catalog, record and delivery
//! vocabulary (STU-CON-003, STU-CON-013, STU-CON-014).
use hsk_studio_accord::{
    ActorContext, BusinessEventDeliveryError, BusinessEventRecord, CausationId, ClientSessionId, CommandContext, CommandId,
    CorrelationId, MAX_BUSINESS_EVENT_ARTIFACTS, StudioBusinessEventId,
};
use std::collections::BTreeSet;

const DOC: &str = "SDOC-019abcde-0000-7000-8000-000000000001";
const LAYER: &str = "SLYR-019abcde-0000-7000-8000-0000000000a1";

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

fn context() -> CommandContext {
    let actor = ActorContext::new("account-7", "principal-7", "owner", "owner-principal", "space", "session-7").unwrap();
    CommandContext::new(
        actor,
        ClientSessionId::parse("client-1").unwrap(),
        Some(CommandId::parse("layer.rename").unwrap()),
        3,
        CorrelationId::new(1001),
        Some(CausationId::new(1000)),
    )
}

#[test]
fn business_event_catalog_equals_the_spec_table() {
    let rows: Vec<String> = StudioBusinessEventId::ALL
        .iter()
        .map(|v| format!("| `{}` | `{}` | `{}` |\n", v.as_str(), v.family(), v.business_id()))
        .collect();
    assert_eq!(rows.concat(), SPEC_TABLE);
    let wire: BTreeSet<&str> = StudioBusinessEventId::ALL.iter().map(|v| v.as_str()).collect();
    let ids: BTreeSet<&str> = StudioBusinessEventId::ALL.iter().map(|v| v.business_id()).collect();
    assert_eq!((wire.len(), ids.len()), (22, 22), "wire names and business ids are distinct");
    for v in StudioBusinessEventId::ALL {
        assert_eq!(StudioBusinessEventId::parse(v.as_str()), Some(v));
        assert_eq!(StudioBusinessEventId::parse(&v.as_str().to_lowercase()), None, "wire names are exact");
    }
    // STU-CON-014: selection, proposal and rejection never claim a document promotion.
    let no_promotion: Vec<StudioBusinessEventId> =
        StudioBusinessEventId::ALL.into_iter().filter(|v| !v.claims_document_promotion()).collect();
    assert_eq!(
        no_promotion,
        [StudioBusinessEventId::SelectionChanged, StudioBusinessEventId::EditProposalRecorded, StudioBusinessEventId::EditRejected]
    );
}

#[test]
fn records_carry_ids_only_and_refuse_typed() {
    let record = BusinessEventRecord::new(StudioBusinessEventId::LayerMutated, DOC, 4, 0, context())
        .unwrap()
        .with_artifacts(&[LAYER])
        .unwrap();
    assert_eq!(record.event_id(), format!("{DOC}@4#0"));
    assert_eq!((record.revision(), record.sequence(), record.context(), record.artifacts()[0].as_str()), (4, 0, &context(), LAYER));
    // The document is an SDOC id; a layer id is refused, an invalid id too.
    assert_eq!(BusinessEventRecord::new(StudioBusinessEventId::LayerMutated, LAYER, 4, 0, context()).unwrap_err().code(), "prefix_mismatch");
    assert_eq!(
        BusinessEventRecord::new(StudioBusinessEventId::LayerMutated, "Quarterly Secret Poster", 4, 0, context()).unwrap_err().code(),
        "invalid_id"
    );
    // Artifact refs: bounded before copying, typed ids only, no repeats.
    let base = BusinessEventRecord::new(StudioBusinessEventId::LayerMutated, DOC, 4, 0, context()).unwrap();
    let many: Vec<String> = (0..=MAX_BUSINESS_EVENT_ARTIFACTS).map(|i| format!("SLYR-019abcde-0000-7000-8000-{i:012x}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    assert_eq!(base.clone().with_artifacts(&many).unwrap_err().code(), "item_limit");
    assert!(base.clone().with_artifacts(&many[..MAX_BUSINESS_EVENT_ARTIFACTS]).is_ok());
    assert_eq!(base.clone().with_artifacts(&[LAYER, LAYER]).unwrap_err().code(), "invalid_id");
    assert_eq!(base.with_artifacts(&["work/poster.psd"]).unwrap_err().code(), "invalid_id");
    // Delivery errors: closed observe semantics, Indeterminate alone needs reconciliation.
    let all = [
        BusinessEventDeliveryError::Saturated,
        BusinessEventDeliveryError::Rejected,
        BusinessEventDeliveryError::Unavailable,
        BusinessEventDeliveryError::Indeterminate,
    ];
    let codes: Vec<(&str, bool)> = all.iter().map(|e| (e.code(), e.needs_reconciliation())).collect();
    assert_eq!(codes, [("saturated", false), ("rejected", false), ("unavailable", false), ("indeterminate", true)]);
}
