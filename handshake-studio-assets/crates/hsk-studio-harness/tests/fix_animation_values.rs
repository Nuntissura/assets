//! Lane SMB-FIX-ANIMATION-VALUES-1 ([STU-MOT-038], [STU-MOT-020b]): enabling animation keys the
//! current evaluated value, and disabling it returns exactly the prior static value. The first two
//! tests are the round R60 attempt-3 probes `r60a3g_motion_values` (SMB-MOTION-COMMANDS-1), ported
//! unchanged.
use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::{Harness, Run, Selection, asset_local_test_context};
use serde_json::{Value, json};

const FIXTURE: &[u8] = include_bytes!("fixtures/demo.handshake");
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";
const S: u64 = 254_016_000_000;

fn cmd(run: &mut Run<'_>, id: &str, params: Value) -> Value {
    assert!(run.motion_command(id, &params), "{id} {params}: {}", run.ops().last().unwrap());
    run.ops().last().unwrap()["detail"].clone()
}

fn has_anchor(run: &Run<'_>) -> bool {
    let c: Value = serde_json::from_slice(run.document_entry("composition").unwrap()).unwrap();
    let l = c["compositions"][0]["layers"].as_array().unwrap().iter().find(|l| l["layer_id"] == L2).unwrap().clone();
    l["streams"].as_array().into_iter().flatten().any(|s| s["property"] == "anchor")
}

fn with_layer(f: impl FnOnce(&mut Run<'_>)) {
    let h = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
    let cancel = CancellationToken::default();
    let mut run = h.run(asset_local_test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    cmd(&mut run, "composition.new", json!({"composition_id": "main", "name": "main", "width": 64, "height": 32, "frame_rate": {"numerator": 25, "denominator": 1}, "duration": 12 * S}));
    cmd(&mut run, "composition.add_layer", json!({"composition_id": "main", "kind": "existing", "layer_id": L2}));
    f(&mut run);
}

#[test]
fn r60a3g_enable_animation_keys_the_current_value() {
    with_layer(|run| {
        cmd(run, "property.set_value", json!({"layer_id": L2, "property": "opacity", "value": [40.0]}));
        if run.motion_command("property.enable_animation", &json!({"layer_id": L2, "property": "opacity", "time": S, "value": [90.0]})) {
            let v = cmd(run, "property.read", json!({"layer_id": L2, "property": "opacity", "time": 3 * S}))["value_at"].clone();
            assert_eq!(v, json!([40.0]), "enable animation changed the value (STU-MOT-038)");
        }
    });
}

#[test]
fn r60a3g_disable_animation_returns_an_absent_anchor() {
    with_layer(|run| {
        assert!(!has_anchor(run));
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "anchor", "time": 0, "value": [37.0, 11.0]}));
        cmd(run, "property.disable_animation", json!({"layer_id": L2, "property": "anchor"}));
        assert!(!has_anchor(run), "STU-MOT-020b: disabling left the caller's value as a stored static anchor");
    });
}

fn comps(run: &Run<'_>) -> Value {
    serde_json::from_slice(run.document_entry("composition").unwrap()).unwrap()
}

fn stream(run: &Run<'_>, property: &str) -> Option<Value> {
    let c = comps(run);
    let l = c["compositions"][0]["layers"].as_array().unwrap().iter().find(|l| l["layer_id"] == L2).unwrap().clone();
    l["streams"].as_array().into_iter().flatten().find(|s| s["property"] == property).cloned()
}

fn undo_len(run: &Run<'_>) -> usize {
    run.history()["undo"].as_array().unwrap().len()
}

fn last_label(run: &Run<'_>) -> String {
    run.history()["undo"].as_array().unwrap().last().unwrap().as_str().unwrap().to_string()
}

/// A refused command: its typed code and target; asserts the entries and the history did not change.
fn refused(run: &mut Run<'_>, id: &str, params: Value) -> (String, Value) {
    let before = (run.document_entry("composition").map(<[u8]>::to_vec), run.document_entry("motion").map(<[u8]>::to_vec));
    let depth = undo_len(run);
    assert!(!run.motion_command(id, &params), "{id} {params} was accepted");
    let after = (run.document_entry("composition").map(<[u8]>::to_vec), run.document_entry("motion").map(<[u8]>::to_vec));
    assert_eq!(before, after, "{id}: a refusal changes nothing");
    assert_eq!(undo_len(run), depth, "{id}: a refusal adds no history step");
    let op = run.ops().last().unwrap().clone();
    (op["error"].as_str().unwrap().to_string(), op["target"].clone())
}

fn value_at(run: &mut Run<'_>, property: &str, time: u64) -> Value {
    cmd(run, "property.read", json!({"layer_id": L2, "property": property, "time": time}))["value_at"].clone()
}

/// Saves through a checkpoint and reopens: the records come back unchanged and still read.
fn reopened(run: &mut Run<'_>, f: impl FnOnce(&mut Run<'_>)) {
    let want = comps(run);
    let bytes = run.checkpoint().expect("checkpoint");
    let h = Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap();
    let cancel = CancellationToken::default();
    let mut again = h.run(asset_local_test_context(), &cancel);
    assert!(again.open_bytes(&bytes), "{:?}", again.ops().last());
    assert_eq!(comps(&again), want);
    f(&mut again);
}

#[test]
fn enable_animation_refuses_a_callers_value_and_keys_the_current_value_as_one_undo_step() {
    with_layer(|run| {
        cmd(run, "property.set_value", json!({"layer_id": L2, "property": "opacity", "value": [40.0]}));
        // The row declares no `value`: refused before planning, never applied and never ignored.
        let (code, target) = refused(run, "property.enable_animation", json!({"layer_id": L2, "property": "opacity", "time": S, "value": [90.0]}));
        assert_eq!((code.as_str(), target), ("parameter_unknown", json!("value")));
        let before = comps(run);
        let depth = undo_len(run);
        cmd(run, "property.enable_animation", json!({"layer_id": L2, "property": "opacity", "time": S}));
        assert_eq!(undo_len(run), depth + 1);
        assert_eq!(last_label(run), "Enable Animation");
        let s = stream(run, "opacity").unwrap();
        assert_eq!(s["static_value"], json!([40.0]), "the static value is retained (STU-MOT-020b)");
        assert_eq!(s["keyframes"].as_array().unwrap().len(), 1);
        assert_eq!((s["keyframes"][0]["time"].clone(), s["keyframes"][0]["value"].clone()), (json!(S), json!([40.0])));
        for t in [0, S, 3 * S] {
            assert_eq!(value_at(run, "opacity", t), json!([40.0]), "enabling animation changed the value at {t}");
        }
        let after = comps(run);
        assert!(run.undo());
        assert_eq!(comps(run), before);
        assert!(run.redo());
        assert_eq!(comps(run), after);
        assert_eq!(refused(run, "property.enable_animation", json!({"layer_id": L2, "property": "opacity", "time": 2 * S})).0, "property_already_animated");
        // An absent anchor's current value derives from the content, so it is not knowable here:
        // refused typed, never seeded.
        assert_eq!(refused(run, "property.enable_animation", json!({"layer_id": L2, "property": "anchor", "time": 0})).0, "property_default_unknown");
        reopened(run, |again| {
            assert_eq!(value_at(again, "opacity", 3 * S), json!([40.0]));
            cmd(again, "property.disable_animation", json!({"layer_id": L2, "property": "opacity"}));
            assert_eq!(stream(again, "opacity").unwrap(), json!({"property": "opacity", "static_value": [40.0]}));
        });
    });
}
