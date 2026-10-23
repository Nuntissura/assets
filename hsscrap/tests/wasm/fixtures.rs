#[path = "../gui/fixture.rs"]
pub mod owner_fixture;

use hsscrap::interaction::SaveRequest;
use hsscrap::operations::{finish_operation, OwnerStepReceipt};
use hsscrap::*;
use hsscrap_wasm::{wire::InspectionWire, HsscrapBridge};
use owner_fixture::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const INSTANCE: &str = "00000000-0000-0000-0000-000000000071";
pub const OTHER_INSTANCE: &str = "00000000-0000-0000-0000-000000000072";

pub fn initial(instance: &str) -> String {
    let mut inspection = inspection();
    inspection.item.revision = Revision(9_007_199_254_740_997);
    inspection.owner_fences[0].revision = inspection.item.revision;
    json!({"version":1,"instance_id":instance,"inspection":InspectionWire::from(inspection)})
        .to_string()
}

pub fn command(action: Value) -> String {
    json!({"version":1,"action":action}).to_string()
}

pub fn save_context() -> hsscrap::ports::CommandContext {
    let mut value = context(90);
    value.expected_revisions[0].revision = Revision(9_007_199_254_740_997);
    value
}

pub fn committed(request: &SaveRequest) -> OperationOutcome {
    let owners: Vec<_> = request
        .plan
        .commands
        .iter()
        .enumerate()
        .map(|(index, _)| host("owner", &index.to_string()))
        .collect();
    let receipts: Vec<_> = owners
        .iter()
        .enumerate()
        .map(|(index, owner)| OwnerStepReceipt {
            operation_id: request.ticket.operation_id,
            intent_digest: request.plan.context.intent_digest.clone(),
            command_index: index,
            owner: owner.clone(),
            receipt: host("receipt", &index.to_string()),
        })
        .collect();
    finish_operation(&request.plan, &receipts, &owners, vec![], at()).unwrap()
}

#[derive(Serialize, Deserialize)]
pub struct Step {
    pub method: String,
    pub input: String,
    pub ok: Option<Value>,
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Case {
    pub name: String,
    pub initial: String,
    pub initial_error: Option<String>,
    pub steps: Vec<Step>,
}

fn record(
    bridge: &mut HsscrapBridge,
    case: &mut Case,
    method: &str,
    input: String,
) -> Option<Value> {
    let result = match method {
        "apply" => bridge.apply(&input),
        "panel" => bridge.panel(&input),
        "snapshot" => bridge.snapshot(),
        _ => panic!("unknown fixture method"),
    };
    let (ok, error) = match result {
        Ok(value) => (Some(serde_json::from_str::<Value>(&value).unwrap()), None),
        Err(error) => (None, Some(error)),
    };
    case.steps.push(Step {
        method: method.into(),
        input,
        ok: ok.clone(),
        error,
    });
    ok
}

pub fn comparison_cases() -> Vec<Case> {
    let mut cases = vec![];
    let input = initial(INSTANCE);
    let mut bridge = HsscrapBridge::new(&input).unwrap();
    let mut case = Case {
        name: "edit_tag_replay".into(),
        initial: input.clone(),
        initial_error: None,
        steps: vec![],
    };
    record(&mut bridge, &mut case, "snapshot", String::new());
    record(
        &mut bridge,
        &mut case,
        "panel",
        json!({"version":1,"width":390,"height":844}).to_string(),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"begin_edit"})),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"set_title","value":Override::Set("Browser draft".to_string())})),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"add_tag","assignment":assignment(81,"later",project())})),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(
            json!({"type":"remove_tag","assignment_id":AssignmentId(uuid::Uuid::from_u128(22))}),
        ),
    );
    let saving = record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"begin_save","context":save_context()})),
    )
    .unwrap();
    let token = saving["view"]["pending"]["token"]
        .as_str()
        .unwrap()
        .to_string();
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"complete","token":token,"result":{"type":"unknown"}})),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"begin_save","context":save_context()})),
    );
    record(&mut bridge, &mut case, "snapshot", String::new());
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"complete","token":token,"result":{"type":"conflict"}})),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(
            json!({"type":"reload","inspection":serde_json::from_str::<Value>(&input).unwrap()["inspection"]}),
        ),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"complete","token":token,"result":{"type":"unknown"}})),
    );
    cases.push(case);

    let mut bridge = HsscrapBridge::new(&input).unwrap();
    let mut case = Case {
        name: "committed_and_malformed_receipt".into(),
        initial: input.clone(),
        initial_error: None,
        steps: vec![],
    };
    let mut direct = {
        let wire: Value = serde_json::from_str(&input).unwrap();
        let inspection: InspectionWire =
            serde_json::from_value(wire["inspection"].clone()).unwrap();
        hsscrap::interaction::Session::new(inspection.into_native().unwrap()).unwrap()
    };
    direct.begin_edit().unwrap();
    direct
        .set_title(Override::Set("Saved title".into()))
        .unwrap();
    let request = direct
        .begin_save(save_context(), &std::collections::BTreeSet::new())
        .unwrap();
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"begin_edit"})),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"set_title","value":Override::Set("Saved title".to_string())})),
    );
    let saving = record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"begin_save","context":save_context()})),
    )
    .unwrap();
    assert_eq!(
        saving["view"]["pending"]["plan"],
        serde_json::to_value(&request.plan).unwrap()
    );
    let token = saving["view"]["pending"]["token"].clone();
    let mut invalid = committed(&request);
    invalid.owner_receipt_refs.clear();
    assert!(direct.complete(request.ticket, Ok(invalid.clone())));
    let uncertain = record(
        &mut bridge,
        &mut case,
        "apply",
        command(
            json!({"type":"complete","token":token,"result":{"type":"outcome","outcome":invalid}}),
        ),
    )
    .unwrap();
    assert!(matches!(
        direct.phase(),
        hsscrap::interaction::Phase::Unknown(_)
    ));
    assert_eq!(uncertain["view"]["phase"], "unknown");
    let outcome = committed(&request);
    assert!(direct.complete(request.ticket, Ok(outcome.clone())));
    let accepted = record(
        &mut bridge,
        &mut case,
        "apply",
        command(
            json!({"type":"complete","token":token,"result":{"type":"outcome","outcome":outcome}}),
        ),
    )
    .unwrap();
    assert_eq!(direct.phase(), &hsscrap::interaction::Phase::ReloadRequired);
    assert_eq!(accepted["view"]["phase"], "reload_required");
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"complete","token":token,"result":{"type":"unknown"}})),
    );
    cases.push(case);

    let mut bridge = HsscrapBridge::new(&input).unwrap();
    let mut case = Case {
        name: "revoked_reused_operation".into(),
        initial: input.clone(),
        initial_error: None,
        steps: vec![],
    };
    for action in [
        json!({"type":"begin_edit"}),
        json!({"type":"set_description","value":Override::<String>::Inherit}),
        json!({"type":"begin_save","context":save_context()}),
    ] {
        record(&mut bridge, &mut case, "apply", command(action));
    }
    let old = serde_json::from_str::<Value>(&bridge.snapshot().unwrap()).unwrap()["view"]
        ["pending"]["token"]
        .clone();
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"revoke"})),
    );
    record(
        &mut bridge,
        &mut case,
        "panel",
        json!({"version":1,"width":390,"height":844}).to_string(),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(
            json!({"type":"reload","inspection":serde_json::from_str::<Value>(&input).unwrap()["inspection"]}),
        ),
    );
    for action in [
        json!({"type":"begin_edit"}),
        json!({"type":"set_description","value":Override::<String>::Inherit}),
        json!({"type":"begin_save","context":save_context()}),
    ] {
        record(&mut bridge, &mut case, "apply", command(action));
    }
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"complete","token":old,"result":{"type":"unknown"}})),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(
            json!({"type":"complete","token":format!("{OTHER_INSTANCE}:2"),"result":{"type":"unknown"}}),
        ),
    );
    record(&mut bridge, &mut case, "snapshot", String::new());
    cases.push(case);

    let mut bridge = HsscrapBridge::new(&input).unwrap();
    let mut case = Case {
        name: "bounds_and_cancel".into(),
        initial: input.clone(),
        initial_error: None,
        steps: vec![],
    };
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"begin_edit"})),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(
            json!({"type":"set_title","value":Override::Set("x".repeat(bounds().max_text_bytes+1))}),
        ),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(
            json!({"type":"add_tag","assignment":assignment(82,"later",TagTarget::Item(ItemId(uuid::Uuid::from_u128(999))))}),
        ),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        command(json!({"type":"cancel"})),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        "{\"version\":1,\"version\":1,\"action\":{\"type\":\"revoke\"}}".into(),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        json!({"version":2,"action":{"type":"revoke"}}).to_string(),
    );
    record(
        &mut bridge,
        &mut case,
        "apply",
        "x".repeat(hsscrap_wasm::MAX_INPUT_BYTES + 1),
    );
    record(
        &mut bridge,
        &mut case,
        "panel",
        json!({"version":1,"width":100000,"height":844}).to_string(),
    );
    record(&mut bridge, &mut case, "snapshot", String::new());
    cases.push(case);

    let mut unsupported: Value = serde_json::from_str(&input).unwrap();
    unsupported["inspection"]["item"]["required_features"] = json!(["future:unknown"]);
    let mut too_large: Value = serde_json::from_str(&input).unwrap();
    too_large["inspection"]["bounds"]["max_collection_items"] = json!(1025);
    let mut deep: Value = serde_json::from_str(&input).unwrap();
    let mut nested = json!(null);
    for _ in 0..129 {
        nested = json!([nested]);
    }
    deep["inspection"]["item"]["extensions"] = json!({"test.opaque":nested});
    for (name, input) in [
        ("unknown_required_feature", unsupported.to_string()),
        ("bounds_ceiling", too_large.to_string()),
        ("depth_ceiling", deep.to_string()),
        (
            "unknown_version",
            json!({"version":99,"instance_id":INSTANCE,"inspection":unsupported["inspection"]})
                .to_string(),
        ),
    ] {
        let error = HsscrapBridge::new(&input)
            .err()
            .expect("negative constructor fixture");
        cases.push(Case {
            name: name.into(),
            initial: input,
            initial_error: Some(error),
            steps: vec![],
        });
    }
    for (name, literal) in [
        ("unsafe_integer", "9007199254740992"),
        ("unsafe_decimal", "9007199254740992.0"),
        ("unsafe_exponent", "9.007199254740992e15"),
        ("unsafe_negative", "-9007199254740992.0"),
    ] {
        let mut value: Value = serde_json::from_str(&input).unwrap();
        value["inspection"]["item"]["extensions"] =
            json!({"test.integer":serde_json::from_str::<Value>(literal).unwrap()});
        let input = value.to_string();
        let error = HsscrapBridge::new(&input)
            .err()
            .expect("unsafe integer rejected");
        assert_eq!(error, "numeric_range");
        cases.push(Case {
            name: name.into(),
            initial: input,
            initial_error: Some(error),
            steps: vec![],
        });
    }
    let mut precise: Value = serde_json::from_str(&input).unwrap();
    precise["inspection"]["item"]["extensions"] = json!({"test.opaque":{"ordered":[null,"retained",1],"safe_integer":9_007_199_254_740_991_u64,"large_decimal":"9007199254740997"}});
    let mut within_depth = json!(null);
    for _ in 0..65 {
        within_depth = json!([within_depth]);
    }
    precise["inspection"]["item"]["extensions"]["test.within_depth"] = within_depth;
    let input = precise.to_string();
    let mut bridge = HsscrapBridge::new(&input).unwrap();
    let mut case = Case {
        name: "precise_integer_strings".into(),
        initial: input,
        initial_error: None,
        steps: vec![],
    };
    record(&mut bridge, &mut case, "snapshot", String::new());
    cases.push(case);
    cases
}
