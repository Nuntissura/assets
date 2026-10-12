//! Owning native consumers for the six frozen WP013 cases. Browser evidence is separate.
use kumihimo_web::{PilotConfig, PilotHost};
use serde_json::{json, Value};

fn pilot() -> PilotHost {
    let config: PilotConfig = serde_json::from_value(json!({
        "task_count": 4, "detail_count": 2, "nesting_depth": 2,
        "width": 980.0, "height": 720.0, "max_records": 4096
    }))
    .unwrap();
    PilotHost::new("native-proof-a", config).unwrap()
}
fn route(host: &mut PilotHost, request: Value) -> Value {
    serde_json::from_str(&host.route_json(&request.to_string())).unwrap()
}
fn read(host: &mut PilotHost, name: &str) -> Value {
    let response = route(host, json!({"version": 1, "route": name}));
    assert_eq!(response["version"], 1);
    assert_eq!(response["route"], name);
    assert_eq!(response["ok"], true, "{response}");
    response["result"].clone()
}
fn snapshot(host: &mut PilotHost) -> Value {
    read(host, "snapshot")
}
fn command(host: &mut PilotHost, action: Value) -> Value {
    let current = snapshot(host);
    route(
        host,
        json!({"version":1,"route":"command", "identity":current["identity"],
        "actor":"native-proof", "operation":current["next_operation"],
        "expected_generation":current["generation"], "action":action}),
    )
}
fn host_command(host: &mut PilotHost, task: &Value, action: Value) -> Value {
    let current = snapshot(host);
    let domain = read(host, "host_snapshot");
    route(
        host,
        json!({"version":1,"route":"host_command", "identity":current["identity"],
        "actor":"native-proof", "expected_revision":domain["revision"],
        "view":task["views"][0], "resource":task["resource"], "provider":task["provider"],
        "action":action}),
    )
}
fn membership(state: &Value) -> Value {
    json!({"root":state["root"],"workspaces":state["workspaces"].as_array().unwrap().iter().map(|w| {
            json!({"id":w["id"],"owner":w["owner"],"frames":w["frames"]})
        }).collect::<Vec<_>>(),
        "views":state["views"].as_array().unwrap().iter().map(|v| {
            json!({"id":v["id"],"frame":v["frame"],"resource":v["resource"],"provider":v["provider"],"group":v["group"]})
        }).collect::<Vec<_>>(),"resources":state["resources"],
        "frames": state["frames"].as_array().unwrap().iter().map(|frame| {
            json!({"id":frame["id"],"workspace":frame["workspace"],
                "child_workspace":frame["child_workspace"],"views":frame["views"]})
        }).collect::<Vec<_>>()})
}
fn root_frame(state: &Value) -> Value {
    state["frames"]
        .as_array()
        .unwrap()
        .iter()
        .find(|frame| frame["workspace"] == state["root"])
        .unwrap()
        .clone()
}
fn float_action(frame: &Value) -> Value {
    // Preserve measured recursive minimums; this checks floating semantics rather
    // than inventing a small rectangle that cannot contain the configured tree.
    json!({"kind":"float","frame":frame["id"],"rect":frame["rect"]})
}

#[test]
fn case001_variable_nested_geometry_preserves_membership() {
    let mut host = pilot();
    let before = snapshot(&mut host);
    assert!(before["frames"].as_array().unwrap().len() > 4);
    assert!(before["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["kind"] == "split"));
    assert!(before["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n["kind"] == "grid"));
    assert!(!before["groups"].as_array().unwrap().is_empty());
    assert!(before["frames"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["depth"].as_u64().unwrap() >= 2));
    for frame in before["frames"].as_array().unwrap() {
        assert!(frame["id"].as_str().unwrap().parse::<u64>().is_ok());
        for field in ["rect", "content", "clip"] {
            for coordinate in ["x", "y", "width", "height"] {
                let value = frame[field][coordinate].as_f64().unwrap();
                assert!(value.is_finite());
                if coordinate == "width" || coordinate == "height" {
                    assert!(value >= 0.0);
                }
            }
        }
    }
    let response = command(
        &mut host,
        json!({"kind":"viewport","rect":{"x":47.0,"y":63.0,"width":180.0,"height":130.0},"scale":1.25}),
    );
    assert_eq!(response["ok"], true, "{response}");
    let narrow = snapshot(&mut host);
    assert_eq!(membership(&narrow), membership(&before));
    assert_eq!(narrow["scale"], 1.25);
    assert!(narrow["frames"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["rect"]["x"].as_f64().unwrap() >= 47.0));
    assert_eq!(
        command(
            &mut host,
            json!({"kind":"viewport","rect":{"x":47.0,"y":63.0,"width":980.0,"height":720.0},"scale":1.0})
        )["ok"],
        true
    );
    assert_eq!(membership(&snapshot(&mut host)), membership(&before));
}

#[test]
fn case002_preview_cancel_stale_geometry_and_input_recipient() {
    let mut host = pilot();
    let before = snapshot(&mut host);
    let frame = root_frame(&before);
    let action = float_action(&frame);
    let preview = route(
        &mut host,
        json!({"version":1,"route":"preview","identity":before["identity"],"expected_generation":before["generation"],"action":action}),
    );
    assert_eq!(preview["ok"], true, "{preview}");
    assert_eq!(snapshot(&mut host), before, "preview must not commit");
    assert_eq!(
        route(
            &mut host,
            json!({"version":1,"route":"cancel_preview","token":preview["result"]["token"]})
        )["ok"],
        true
    );
    assert_eq!(snapshot(&mut host), before);
    assert_eq!(
        command(
            &mut host,
            json!({"kind":"viewport","rect":{"x":40.0,"y":60.0,"width":980.0,"height":720.0},"scale":1.25})
        )["ok"],
        true
    );
    let stale = route(
        &mut host,
        json!({"version":1,"route":"preview","identity":before["identity"],"expected_generation":before["generation"],"action":action}),
    );
    assert_eq!(stale["ok"], false);
    let state = snapshot(&mut host);
    let task = read(&mut host, "host_snapshot")["tasks"][0].clone();
    let surface = state["frames"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["selected_view"] == task["views"][0])
        .unwrap();
    let input = json!({"version":1,"route":"input","identity":state["identity"],"epoch":state["epoch"],
        "view":task["views"][0],"resource":task["resource"],"provider":task["provider"],
        "x":surface["content"]["x"].as_f64().unwrap()+2.0,"y":surface["content"]["y"].as_f64().unwrap()+3.0,
        "scale":state["scale"],"payload":"pointer"});
    let mapped = route(&mut host, input.clone());
    assert_eq!(mapped["ok"], true, "{mapped}");
    assert_eq!(mapped["result"]["local"], json!({"x":2.0,"y":3.0}));
    assert_eq!(mapped["result"]["recipient"]["view"], task["views"][0]);
    assert_eq!(mapped["result"]["recipient"]["resource"], task["resource"]);
    assert_eq!(mapped["result"]["recipient"]["provider"], task["provider"]);
    let mut stale_input = input;
    stale_input["epoch"] = json!("0");
    assert_eq!(route(&mut host, stale_input)["ok"], false);
}

#[test]
fn case003_domain_owner_recipient_provider_and_veto() {
    let mut host = pilot();
    let domain = read(&mut host, "host_snapshot");
    let task = domain["tasks"][0].clone();
    let state = snapshot(&mut host);
    let frame = root_frame(&state);
    assert_eq!(
        command(
            &mut host,
            json!({"kind":"expand","frame":frame["id"],"expanded":false})
        )["ok"],
        true
    );
    assert_eq!(read(&mut host, "host_snapshot"), domain);
    assert_eq!(
        host_command(
            &mut host,
            &task,
            json!({"kind":"edit","title":"Edited owning task","body":"Real host text"})
        )["ok"],
        true
    );
    let edited = read(&mut host, "host_snapshot");
    assert_eq!(edited["tasks"][0]["title"], "Edited owning task");
    assert_eq!(edited["tasks"][1], domain["tasks"][1]);
    assert_eq!(
        host_command(
            &mut host,
            &task,
            json!({"kind":"domain","column":"done","status":"complete"})
        )["ok"],
        true
    );
    assert_eq!(
        read(&mut host, "host_snapshot")["tasks"][0]["column"],
        "done"
    );
    let mut wrong = task.clone();
    wrong["resource"] = domain["tasks"][1]["resource"].clone();
    let valid = read(&mut host, "host_snapshot");
    assert_eq!(
        host_command(
            &mut host,
            &wrong,
            json!({"kind":"edit","title":"wrong","body":"wrong"})
        )["ok"],
        false
    );
    assert_eq!(read(&mut host, "host_snapshot"), valid);
    assert_eq!(
        host_command(&mut host, &task, json!({"kind":"veto","enabled":true}))["ok"],
        true
    );
    let owned = membership(&snapshot(&mut host));
    assert_eq!(
        command(
            &mut host,
            json!({"kind":"close","view":task["views"][0],"approved":false})
        )["ok"],
        false
    );
    assert_eq!(membership(&snapshot(&mut host)), owned);
    assert_eq!(
        command(
            &mut host,
            json!({"kind":"provider_availability","provider":task["provider"],"available":false})
        )["ok"],
        true
    );
    let unavailable = snapshot(&mut host);
    assert_eq!(membership(&unavailable), owned);
}

#[test]
fn case004_candidate_load_preserves_valid_state_and_fresh_identity() {
    let mut host = pilot();
    let frame = root_frame(&snapshot(&mut host));
    assert_eq!(command(&mut host, float_action(&frame))["ok"], true);
    let saved = read(&mut host, "save");
    let wrapper: Value = serde_json::from_str(saved["layout"].as_str().unwrap()).unwrap();
    assert_eq!(wrapper["version"], 1);
    let old = snapshot(&mut host);
    let domain = read(&mut host, "host_snapshot");
    for bad in [
        "{".to_string(),
        "x".repeat(2_000_000),
        json!({"version":2,"layout":wrapper["layout"],"expanded":wrapper["expanded"]}).to_string(),
    ] {
        let response = route(
            &mut host,
            json!({"version":1,"route":"load","layout":bad,"host":saved["host"],"fresh_incarnation":"native-proof-b"}),
        );
        assert_eq!(response["ok"], false);
        assert_eq!(snapshot(&mut host), old);
        assert_eq!(read(&mut host, "host_snapshot"), domain);
    }
    let loaded = route(
        &mut host,
        json!({"version":1,"route":"load","layout":saved["layout"],"host":saved["host"],"fresh_incarnation":"native-proof-b"}),
    );
    assert_eq!(loaded["ok"], true, "{loaded}");
    let fresh = snapshot(&mut host);
    assert_ne!(
        fresh["identity"]["incarnation"],
        old["identity"]["incarnation"]
    );
    assert_eq!(membership(&fresh), membership(&old));
    assert_eq!(read(&mut host, "host_snapshot"), domain);
}

#[test]
fn case005_decimal_replay_original_receipt_current_state_and_strict_boundary() {
    let mut host = pilot();
    let first = snapshot(&mut host);
    let large = "9007199254740993";
    let envelope = json!({"version":1,"route":"command","identity":first["identity"],"actor":"model-a",
        "operation":large,"expected_generation":first["generation"],"action":{"kind":"equalize","workspace":first["root"]}});
    let accepted = route(&mut host, envelope.clone());
    assert_eq!(accepted["ok"], true, "{accepted}");
    assert_eq!(accepted["result"]["receipt"]["operation"], large);
    assert_eq!(accepted["result"]["receipt"]["replayed"], false);
    let frame = root_frame(&snapshot(&mut host));
    assert_eq!(command(&mut host, float_action(&frame))["ok"], true);
    let current = snapshot(&mut host);
    let replay = route(&mut host, envelope.clone());
    assert_eq!(replay["ok"], true);
    assert_eq!(replay["result"]["receipt"]["replayed"], true);
    assert_eq!(
        replay["result"]["receipt"]["generation"],
        accepted["result"]["receipt"]["generation"]
    );
    assert_eq!(
        replay["result"]["current"]["generation"],
        current["generation"]
    );
    assert_ne!(
        replay["result"]["receipt"]["generation"],
        current["generation"]
    );
    let mut mismatch = envelope.clone();
    mismatch["actor"] = json!("model-b");
    assert_eq!(route(&mut host, mismatch)["ok"], false);
    let mut different = envelope;
    different["action"] = json!({"kind":"maximize","frame":frame["id"]});
    assert_eq!(route(&mut host, different)["ok"], false);
    assert_eq!(snapshot(&mut host), current);
    let stale_envelope = json!({"version":1,"route":"command","identity":current["identity"],"actor":"model-a",
        "operation":current["next_operation"],"expected_generation":first["generation"],
        "action":{"kind":"equalize","workspace":current["root"]}});
    let rejected = route(&mut host, stale_envelope.clone());
    assert_eq!(rejected["ok"], false);
    assert_eq!(rejected["result"]["receipt"]["replayed"], false);
    let rejected_current = snapshot(&mut host);
    assert_eq!(
        rejected_current["high_watermark"],
        stale_envelope["operation"]
    );
    assert_eq!(rejected_current["generation"], current["generation"]);
    assert_eq!(membership(&rejected_current), membership(&current));
    let rejected_retry = route(&mut host, stale_envelope);
    assert_eq!(rejected_retry["ok"], false);
    assert_eq!(rejected_retry["result"]["receipt"]["replayed"], true);
    assert_eq!(
        rejected_retry["result"]["receipt"]["generation"],
        rejected["result"]["receipt"]["generation"]
    );
    assert_eq!(snapshot(&mut host), rejected_current);
    for bad in [
        json!({"version":1,"route":"snapshot","unknown":true}),
        json!({"version":2,"route":"snapshot"}),
    ] {
        assert_eq!(route(&mut host, bad)["ok"], false);
    }
    let numeric = json!({"version":1,"route":"command","identity":current["identity"],"actor":"model-a",
        "operation":9007199254740993_u64,"expected_generation":current["generation"],"action":{"kind":"equalize","workspace":current["root"]}});
    assert_eq!(
        route(&mut host, numeric)["ok"],
        false,
        "decimal IDs must reject JSON numbers"
    );
    for invalid_json in [
        r#"{"version":1,"version":1,"route":"snapshot"}"#,
        r#"{"version":1,"route":"input","x":NaN}"#,
        r#"{"version":1,"route":"input","x":1e999}"#,
    ] {
        assert_eq!(
            serde_json::from_str::<Value>(&host.route_json(invalid_json)).unwrap()["ok"],
            false
        );
    }
    let invalid = host.route_json(&format!(
        "{{\"version\":1,\"route\":\"snapshot\",\"unknown\":\"{}\"}}",
        "x".repeat(2_000_000)
    ));
    assert_eq!(
        serde_json::from_str::<Value>(&invalid).unwrap()["ok"],
        false
    );
    assert!(!invalid.contains("Real host text"));
    assert_eq!(
        snapshot(&mut host),
        rejected_current,
        "strict boundary failures must not enter Core ledger"
    );
}

#[test]
fn case006_capability_and_invalid_popup_request_cannot_migrate_membership() {
    let mut host = pilot();
    let before = snapshot(&mut host);
    let frame = root_frame(&before);
    let unsupported = command(&mut host, json!({"kind":"popup","frame":frame["id"]}));
    assert_eq!(unsupported["ok"], false);
    assert_eq!(membership(&snapshot(&mut host)), membership(&before));
    let unknown = route(&mut host, json!({"version":1,"route":"unrecognized"}));
    assert_eq!(unknown["ok"], false);
    assert!(unknown["error"].is_object() || unknown["error"].is_string());
    // Actual sandbox user-event denial, browser capabilities, pixels and cleanup are
    // intentionally observed by browser-proof.py rather than simulated here.
}
