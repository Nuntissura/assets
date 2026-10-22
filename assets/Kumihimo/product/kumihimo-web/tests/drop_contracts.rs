#![cfg(feature = "drops")]
use kumihimo_web::{PilotConfig, PilotHost};
use serde_json::{json, Value};

fn route(host: &mut PilotHost, request: Value) -> Value { serde_json::from_str(&host.route_json(&request.to_string())).unwrap() }
fn host() -> PilotHost { PilotHost::new("file-drop-contract", PilotConfig { task_count: 2, detail_count: 0, nesting_depth: 0, ..Default::default() }).unwrap() }
fn snapshot(host: &mut PilotHost) -> Value { route(host, json!({"version":1,"route":"snapshot"}))["result"].clone() }
fn tasks(host: &mut PilotHost) -> Value { route(host, json!({"version":1,"route":"host_snapshot"}))["result"].clone() }
fn admission(host: &mut PilotHost, index: usize, action: &str, size: u64) -> Value {
    let s = snapshot(host);
    let frame = &s["frames"][index];
    let view = s["views"].as_array().unwrap().iter().find(|v| v["id"] == frame["selected_view"]).unwrap();
    route(host, json!({"version":1,"route":"drop_admit","destination":{"identity":s["identity"],"generation":s["generation"],"epoch":s["epoch"],"frame":frame["id"],"view":view["id"],"resource":view["resource"],"provider":view["provider"],"x":frame["content"]["x"].as_f64().unwrap()+2.0,"y":frame["content"]["y"].as_f64().unwrap()+2.0},"action":action,"source":"browser_files","items":[{"label":"fixture.txt","media_type":"text/plain","bytes":size,"asset":null,"asset_revision":null}],"now_ms":10}))
}
#[test]
fn exact_two_targets_and_single_effect_keep_files_host_owned() {
    let mut h = host();
    let a = admission(&mut h, 0, "import", 5); assert_eq!(a["ok"], true);
    let token = a["result"]["token"].clone();
    let done = route(&mut h, json!({"version":1,"route":"drop_complete","token":token,"contents":["first"],"now_ms":20}));
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(done["result"]["recipient"]["frame"], snapshot(&mut h)["frames"][0]["id"]);
    let b = admission(&mut h, 1, "copy", 6); assert_eq!(b["ok"], true);
    let other = route(&mut h, json!({"version":1,"route":"drop_complete","token":b["result"]["token"],"contents":["second"],"now_ms":20}));
    assert_eq!(other["ok"], true, "{other}");
    let t = tasks(&mut h);
    assert!(t["tasks"][0]["body"].as_str().unwrap().ends_with("first"));
    assert_eq!(t["tasks"][1]["body"], "second");
    let revision = t["revision"].clone();
    assert_eq!(route(&mut h, json!({"version":1,"route":"drop_complete","token":token,"contents":["first"],"now_ms":20}))["ok"], false);
    assert_eq!(tasks(&mut h)["revision"], revision);
}
#[test]
fn cancelled_expired_and_oversized_transfer_preserve_document() {
    let mut h = host();
    let original = tasks(&mut h);
    let a = admission(&mut h, 0, "open", 3); assert_eq!(a["ok"], true);
    assert_eq!(route(&mut h, json!({"version":1,"route":"drop_cancel","token":a["result"]["token"]}))["ok"], true);
    assert_eq!(route(&mut h, json!({"version":1,"route":"drop_complete","token":a["result"]["token"],"contents":["abc"],"now_ms":20}))["ok"], false);
    let b = admission(&mut h, 0, "open", 3); assert_eq!(b["ok"], true);
    assert_eq!(route(&mut h, json!({"version":1,"route":"drop_complete","token":b["result"]["token"],"contents":["abc"],"now_ms":30010}))["ok"], false);
    assert_eq!(admission(&mut h, 0, "move", 3)["ok"], false);
    assert_eq!(admission(&mut h, 0, "import", 8193)["ok"], false);
    assert_eq!(tasks(&mut h)["tasks"], original["tasks"]);
}
#[test]
fn canonical_generation_and_host_revision_invalidate_async_read() {
    let mut h = host();
    let a = admission(&mut h, 0, "open", 3); assert_eq!(a["ok"], true);
    let s = snapshot(&mut h);
    let result = route(&mut h, json!({"version":1,"route":"command","identity":s["identity"],"actor":"drop-test","operation":s["next_operation"],"expected_generation":s["generation"],"action":{"kind":"expand","frame":s["frames"][0]["id"],"expanded":false}}));
    assert_eq!(result["ok"], true);
    let before = tasks(&mut h);
    assert_eq!(route(&mut h, json!({"version":1,"route":"drop_complete","token":a["result"]["token"],"contents":["abc"],"now_ms":20}))["ok"], false);
    assert_eq!(tasks(&mut h)["tasks"], before["tasks"]);
}
