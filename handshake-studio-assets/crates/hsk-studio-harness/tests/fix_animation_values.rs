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
