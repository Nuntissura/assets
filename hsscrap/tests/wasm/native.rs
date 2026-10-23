mod fixtures;
use fixtures::owner_fixture::*;
use fixtures::*;
use hsscrap::interaction::{Phase, Session};
use hsscrap::operations::pending_outcome;
use hsscrap::*;
use hsscrap_wasm::{wire::InspectionWire, HsscrapBridge};
use serde_json::{json, Value};
use std::collections::BTreeSet;

fn apply(bridge: &mut HsscrapBridge, action: Value) -> Value {
    serde_json::from_str(&bridge.apply(&command(action)).unwrap()).unwrap()
}
fn native() -> Session {
    let wire: Value = serde_json::from_str(&initial(INSTANCE)).unwrap();
    let inspection: InspectionWire = serde_json::from_value(wire["inspection"].clone()).unwrap();
    Session::new(inspection.into_native().unwrap()).unwrap()
}

#[test]
fn edits_tags_save_and_recovery_match_direct_native_session() {
    let mut session = native();
    let mut bridge = HsscrapBridge::new(&initial(INSTANCE)).unwrap();
    let initial: Value = serde_json::from_str(&bridge.snapshot().unwrap()).unwrap();
    assert_eq!(
        initial["view"]["inspection"]["item"]["revision"],
        "9007199254740997"
    );
    session.begin_edit().unwrap();
    assert_eq!(
        apply(&mut bridge, json!({"type":"begin_edit"}))["view"]["phase"],
        "editing"
    );
    let title = Override::Set("Browser draft".into());
    session.set_title(title.clone()).unwrap();
    let view = apply(&mut bridge, json!({"type":"set_title","value":title}));
    assert_eq!(
        view["view"]["draft"]["title"],
        serde_json::to_value(&session.draft().unwrap().title).unwrap()
    );
    let candidate = assignment(81, "later", project());
    session.stage_tag(candidate.clone()).unwrap();
    let view = apply(
        &mut bridge,
        json!({"type":"add_tag","assignment":candidate}),
    );
    assert_eq!(
        view["view"]["draft"]["additions"],
        serde_json::to_value(&session.draft().unwrap().additions).unwrap()
    );
    let inherited = AssignmentId(uuid::Uuid::from_u128(22));
    session.remove_tag(inherited).unwrap();
    apply(
        &mut bridge,
        json!({"type":"remove_tag","assignment_id":inherited}),
    );
    let request = session
        .begin_save(save_context(), &BTreeSet::new())
        .unwrap();
    let view = apply(
        &mut bridge,
        json!({"type":"begin_save","context":save_context()}),
    );
    assert_eq!(
        view["view"]["pending"]["plan"],
        serde_json::to_value(&request.plan).unwrap()
    );
    assert_eq!(
        view["view"]["pending"]["removal_targets"],
        serde_json::to_value(&request.removal_targets).unwrap()
    );
    assert_eq!(request.removal_targets, vec![(inherited, project())]);
    let token = view["view"]["pending"]["token"].clone();
    session.complete(
        request.ticket,
        Err(Error::UnknownOutcome {
            operation_id: request.ticket.operation_id,
        }),
    );
    let unknown = apply(
        &mut bridge,
        json!({"type":"complete","token":token,"result":{"type":"unknown"}}),
    );
    assert!(matches!(session.phase(), Phase::Unknown(_)));
    assert_eq!(unknown["view"]["phase"], "unknown");
    assert_eq!(unknown["view"]["pending"], view["view"]["pending"]);
    assert!(session
        .begin_save(save_context(), &BTreeSet::new())
        .is_err());
    assert_eq!(
        bridge
            .apply(&command(
                json!({"type":"begin_save","context":save_context()})
            ))
            .unwrap_err(),
        "denied"
    );
    let mut malformed = committed(&request);
    malformed.owner_receipt_refs.clear();
    session.complete(request.ticket, Ok(malformed.clone()));
    let invalid = apply(
        &mut bridge,
        json!({"type":"complete","token":token,"result":{"type":"outcome","outcome":malformed}}),
    );
    assert!(matches!(session.phase(), Phase::Unknown(_)));
    assert_eq!(invalid["view"]["phase"], "unknown");
    let outcome = committed(&request);
    session.complete(request.ticket, Ok(outcome.clone()));
    let accepted = apply(
        &mut bridge,
        json!({"type":"complete","token":token,"result":{"type":"outcome","outcome":outcome}}),
    );
    assert_eq!(session.phase(), &Phase::ReloadRequired);
    assert_eq!(accepted["view"]["phase"], "reload_required");
    assert!(session.draft().is_none());
    assert!(accepted["view"]["draft"].is_null() && accepted["view"]["pending"].is_null());
    assert!(!session.complete(request.ticket, Ok(pending_outcome(&request.plan))));
    assert_eq!(
        bridge
            .apply(&command(
                json!({"type":"complete","token":token,"result":{"type":"unknown"}})
            ))
            .unwrap_err(),
        "stale_ticket"
    );
}

#[test]
fn revoke_reload_and_instance_scoping_reject_old_tickets_and_erase_private_state() {
    let mut session = native();
    let mut bridge = HsscrapBridge::new(&initial(INSTANCE)).unwrap();
    session.begin_edit().unwrap();
    session.set_description(Override::Inherit).unwrap();
    apply(&mut bridge, json!({"type":"begin_edit"}));
    apply(
        &mut bridge,
        json!({"type":"set_description","value":Override::<String>::Inherit}),
    );
    let old = session
        .begin_save(save_context(), &BTreeSet::new())
        .unwrap();
    let saving = apply(
        &mut bridge,
        json!({"type":"begin_save","context":save_context()}),
    );
    let old_token = saving["view"]["pending"]["token"].clone();
    session.revoke();
    let revoked = apply(&mut bridge, json!({"type":"revoke"}));
    assert_eq!(session.phase(), &Phase::Revoked);
    for field in ["inspection", "metadata", "draft", "pending"] {
        assert!(revoked["view"][field].is_null());
    }
    assert!(!bridge.snapshot().unwrap().contains("Browser draft"));
    let fresh = native().inspection().unwrap().clone();
    session.reload(fresh.clone()).unwrap();
    apply(
        &mut bridge,
        json!({"type":"reload","inspection":InspectionWire::from(fresh)}),
    );
    session.begin_edit().unwrap();
    session.set_description(Override::Inherit).unwrap();
    apply(&mut bridge, json!({"type":"begin_edit"}));
    apply(
        &mut bridge,
        json!({"type":"set_description","value":Override::<String>::Inherit}),
    );
    let new = session
        .begin_save(save_context(), &BTreeSet::new())
        .unwrap();
    let saving = apply(
        &mut bridge,
        json!({"type":"begin_save","context":save_context()}),
    );
    assert_ne!(old.ticket, new.ticket);
    assert_ne!(old_token, saving["view"]["pending"]["token"]);
    assert!(!session.complete(old.ticket, Ok(pending_outcome(&old.plan))));
    let before = bridge.snapshot().unwrap();
    assert_eq!(
        bridge
            .apply(&command(
                json!({"type":"complete","token":old_token,"result":{"type":"unknown"}})
            ))
            .unwrap_err(),
        "stale_ticket"
    );
    assert_eq!(bridge.snapshot().unwrap(), before);
    let mut other = HsscrapBridge::new(&initial(OTHER_INSTANCE)).unwrap();
    apply(&mut other, json!({"type":"begin_edit"}));
    apply(
        &mut other,
        json!({"type":"set_description","value":Override::<String>::Inherit}),
    );
    apply(
        &mut other,
        json!({"type":"begin_save","context":save_context()}),
    );
    assert_eq!(other.apply(&command(json!({"type":"complete","token":saving["view"]["pending"]["token"],"result":{"type":"unknown"}}))).unwrap_err(),"stale_ticket");
}

#[test]
fn invalid_wire_and_required_features_fail_before_mutation() {
    let mut bridge = HsscrapBridge::new(&initial(INSTANCE)).unwrap();
    let before = bridge.snapshot().unwrap();
    for input in [
        "{\"version\":1,\"version\":1,\"action\":{\"type\":\"revoke\"}}".to_string(),
        json!({"version":1,"action":{"type":"revoke"},"unknown":true}).to_string(),
        json!({"version":2,"action":{"type":"revoke"}}).to_string(),
        "x".repeat(hsscrap_wasm::MAX_INPUT_BYTES + 1),
    ] {
        assert!(bridge.apply(&input).is_err());
        assert_eq!(bridge.snapshot().unwrap(), before);
    }
    let mut unknown: Value = serde_json::from_str(&initial(INSTANCE)).unwrap();
    unknown["inspection"]["item"]["required_features"] = json!(["future:unknown"]);
    assert_eq!(
        HsscrapBridge::new(&unknown.to_string()).err().unwrap(),
        "unsupported_feature"
    );
    let mut deep: Value = serde_json::from_str(&initial(INSTANCE)).unwrap();
    let mut within_depth = json!(null);
    for _ in 0..65 {
        within_depth = json!([within_depth]);
    }
    deep["inspection"]["item"]["extensions"] = json!({"test.opaque":within_depth});
    let preserved: Value = serde_json::from_str(
        &HsscrapBridge::new(&deep.to_string())
            .unwrap()
            .snapshot()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        preserved["view"]["inspection"]["item"]["extensions"],
        deep["inspection"]["item"]["extensions"]
    );
    let mut nested = json!(null);
    for _ in 0..129 {
        nested = json!([nested]);
    }
    deep["inspection"]["item"]["extensions"] = json!({"test.opaque":nested});
    assert_eq!(
        HsscrapBridge::new(&deep.to_string()).err().unwrap(),
        "invalid_json"
    );
    let mut ceilings: Value = serde_json::from_str(&initial(INSTANCE)).unwrap();
    ceilings["inspection"]["bounds"]["max_operation_bytes"] =
        json!(hsscrap_wasm::MAX_INPUT_BYTES + 1);
    assert_eq!(
        HsscrapBridge::new(&ceilings.to_string()).err().unwrap(),
        "bounds"
    );
    let mut opaque: Value = serde_json::from_str(&initial(INSTANCE)).unwrap();
    opaque["inspection"]["item"]["extensions"] = json!({"test.opaque":{"ordered":[null,"retained",1],"safe_integer":9_007_199_254_740_991_u64,"large_decimal":"9007199254740997"}});
    let accepted = HsscrapBridge::new(&opaque.to_string()).unwrap();
    let output: Value = serde_json::from_str(&accepted.snapshot().unwrap()).unwrap();
    assert_eq!(
        output["view"]["inspection"]["item"]["extensions"],
        opaque["inspection"]["item"]["extensions"]
    );
    for literal in [
        "9007199254740992",
        "9007199254740992.0",
        "9.007199254740992e15",
        "-9007199254740992",
        "-9007199254740992.0",
    ] {
        opaque["inspection"]["item"]["extensions"] =
            json!({"test.integer":serde_json::from_str::<Value>(literal).unwrap()});
        assert_eq!(
            HsscrapBridge::new(&opaque.to_string()).err().unwrap(),
            "numeric_range"
        );
    }
}

#[test]
fn panel_calls_real_gui_and_cancel_remains_a_local_draft_transition() {
    let mut session = native();
    let mut bridge = HsscrapBridge::new(&initial(INSTANCE)).unwrap();
    let before = bridge.snapshot().unwrap();
    let panel: Value = serde_json::from_str(
        &bridge
            .panel(&json!({"version":1,"width":390,"height":844}).to_string())
            .unwrap(),
    )
    .unwrap();
    assert!(panel["controls"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value["id"] == "edit"));
    assert!(panel["actions"].as_array().unwrap().is_empty());
    assert_eq!(bridge.snapshot().unwrap(), before);
    session.begin_edit().unwrap();
    session.set_title(Override::Clear).unwrap();
    apply(&mut bridge, json!({"type":"begin_edit"}));
    apply(
        &mut bridge,
        json!({"type":"set_title","value":Override::<String>::Clear}),
    );
    assert!(session.cancel());
    let cancelled = apply(&mut bridge, json!({"type":"cancel"}));
    assert_eq!(session.phase(), &Phase::Inspect);
    assert_eq!(cancelled["view"]["phase"], "inspect");
    assert!(cancelled["view"]["draft"].is_null() && cancelled["view"]["pending"].is_null());
    assert_eq!(bridge.snapshot().unwrap(), before);
}
