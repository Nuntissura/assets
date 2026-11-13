//! Lane SMB-FIX-ANIMATION-VALUES-1 ([STU-MOT-038], [STU-MOT-020b]): enabling animation keys the
//! current evaluated value, and disabling it returns exactly the prior static value. The first two
//! tests are the round R60 attempt-3 probes `r60a3g_motion_values` (SMB-MOTION-COMMANDS-1), ported
//! unchanged.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Budget, Code, Resolution, Resolver, StudioCompositionRecords, TileRef, validate_composition_records};
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

/// The keyframes of the stream of `property`, as JSON.
fn keys(run: &Run<'_>, property: &str) -> Vec<Value> {
    stream(run, property).and_then(|s| s["keyframes"].as_array().cloned()).unwrap_or_default()
}

#[test]
fn an_implied_anchor_returns_on_disable_while_an_explicit_static_stays_through_undo_and_reopen() {
    with_layer(|run| {
        let before = comps(run);
        // Implied: the key creates the stream, and its static value is marked implied, never taken
        // as the caller's value.
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "anchor", "time": 0, "value": [37.0, 11.0]}));
        let implied = stream(run, "anchor").unwrap();
        assert_eq!(implied["static_implied"], json!(true), "{implied}");
        // Read reports the flag and no static value (the stored one only fills the record; review Q1).
        let read = cmd(run, "property.read", json!({"layer_id": L2, "property": "anchor", "time": 0}));
        assert_eq!((read.get("static_implied"), read.get("static_value")), (Some(&json!(true)), Some(&Value::Null)), "{read}");
        let keyed = comps(run);
        // Round trip with the flag set: it survives, and disabling after reopen still returns to implied.
        reopened(run, |again| {
            assert_eq!(stream(again, "anchor").unwrap()["static_implied"], json!(true));
            cmd(again, "property.disable_animation", json!({"layer_id": L2, "property": "anchor"}));
            assert!(stream(again, "anchor").is_none());
        });
        cmd(run, "property.disable_animation", json!({"layer_id": L2, "property": "anchor"}));
        assert_eq!(last_label(run), "Disable Animation");
        assert_eq!(comps(run), before, "STU-MOT-020b: exactly the prior static value, here the implied anchor");
        assert!(run.undo());
        assert_eq!(comps(run), keyed);
        assert!(run.redo());
        assert_eq!(comps(run), before);
        // Explicit: the same static value and key, from a set value and enable animation. Only the flag
        // tells the two apart, and disabling keeps the explicit static value.
        cmd(run, "property.set_value", json!({"layer_id": L2, "property": "anchor", "value": [37.0, 11.0]}));
        cmd(run, "property.enable_animation", json!({"layer_id": L2, "property": "anchor", "time": 0}));
        let explicit = stream(run, "anchor").unwrap();
        assert_eq!(explicit.get("static_implied"), None, "an unset flag is not on the wire: {explicit}");
        assert_eq!((&explicit["static_value"], &explicit["keyframes"]), (&implied["static_value"], &implied["keyframes"]));
        let read = cmd(run, "property.read", json!({"layer_id": L2, "property": "anchor", "time": 0}));
        assert_eq!((read.get("static_implied"), read.get("static_value")), (None, Some(&json!([37.0, 11.0]))), "{read}");
        // Round trip with the flag unset, the shape a file written before the flag has.
        reopened(run, |again| {
            cmd(again, "property.disable_animation", json!({"layer_id": L2, "property": "anchor"}));
            assert_eq!(stream(again, "anchor").unwrap(), json!({"property": "anchor", "static_value": [37.0, 11.0]}));
        });
        cmd(run, "property.disable_animation", json!({"layer_id": L2, "property": "anchor"}));
        assert_eq!(stream(run, "anchor").unwrap(), json!({"property": "anchor", "static_value": [37.0, 11.0]}));
    });
}

#[test]
fn deleting_or_toggling_the_last_key_of_an_implied_stream_returns_it_to_implied() {
    with_layer(|run| {
        let before = comps(run);
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "anchor", "time": 0, "value": [37.0, 11.0]}));
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "anchor", "time": S, "value": [1.0, 2.0]}));
        cmd(run, "keyframe.delete", json!({"layer_id": L2, "property": "anchor", "time": S}));
        assert_eq!((keys(run, "anchor").len(), stream(run, "anchor").unwrap()["static_implied"].clone()), (1, json!(true)));
        cmd(run, "keyframe.delete", json!({"layer_id": L2, "property": "anchor", "time": 0}));
        assert_eq!(comps(run), before, "the last key's delete returns the anchor to implied");
        assert!(run.undo());
        assert_eq!(keys(run, "anchor").len(), 1);
        assert_eq!(cmd(run, "keyframe.toggle_at_current", json!({"layer_id": L2, "property": "anchor", "time": 0}))["toggled"], "deleted");
        assert_eq!(comps(run), before, "toggling the last key off returns the anchor to implied");
        assert!(run.undo());
        // Paste creates a missing position stream the same way as keyframe.set.
        cmd(run, "keyframe.copy", json!({"layer_id": L2, "property": "anchor", "times": [0]}));
        cmd(run, "keyframe.paste", json!({"layer_id": L2, "property": "position", "time": S}));
        let p = stream(run, "position").unwrap();
        assert_eq!((p["static_implied"].clone(), p["keyframes"][0]["value"].clone()), (json!(true), json!([37.0, 11.0])));
        cmd(run, "property.disable_animation", json!({"layer_id": L2, "property": "position"}));
        assert!(stream(run, "position").is_none());
        // An explicit stream keeps its static value when its last key goes.
        cmd(run, "property.set_value", json!({"layer_id": L2, "property": "opacity", "value": [40.0]}));
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "opacity", "time": S, "value": [90.0]}));
        cmd(run, "keyframe.delete", json!({"layer_id": L2, "property": "opacity", "time": S}));
        assert_eq!(stream(run, "opacity").unwrap(), json!({"property": "opacity", "static_value": [40.0]}));
    });
}

#[test]
fn a_paste_of_another_dimension_onto_an_absent_anchor_is_a_paste_mismatch() {
    with_layer(|run| {
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "rotation", "time": 0, "value": [15.0]}));
        cmd(run, "keyframe.copy", json!({"layer_id": L2, "property": "rotation", "times": [0]}));
        // The anchor is absent: the one-component keys are refused before a stream would be seeded
        // from them (review F3), with the code a present anchor gives.
        let (code, target) = refused(run, "keyframe.paste", json!({"layer_id": L2, "property": "anchor", "time": 0}));
        assert_eq!((code.as_str(), target), ("paste_property_mismatch", json!("anchor")));
        assert!(stream(run, "anchor").is_none());
        let (code, target) = refused(run, "keyframe.paste", json!({"layer_id": L2, "property": "position", "time": S}));
        assert_eq!((code.as_str(), target), ("paste_property_mismatch", json!("position")));
        cmd(run, "property.set_value", json!({"layer_id": L2, "property": "anchor", "value": [1.0, 2.0]}));
        assert_eq!(refused(run, "keyframe.paste", json!({"layer_id": L2, "property": "anchor", "time": 0})).0, "paste_property_mismatch");
    });
}

struct Caller;
impl Resolver for Caller {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        true
    }
}

/// Folio's verdict on the run's composition entry with `edit` applied to the streams of L2.
fn verdict(run: &Run<'_>, edit: impl FnOnce(&mut Value)) -> Result<(), (Code, String)> {
    let mut c = comps(run);
    let l = c["compositions"][0]["layers"].as_array_mut().unwrap().iter_mut().find(|l| l["layer_id"] == L2).unwrap();
    edit(&mut l["streams"]);
    let r: StudioCompositionRecords = serde_json::from_value(c).expect("record shape");
    validate_composition_records(&r, run.snapshot().unwrap().document(), Budget::default(), &CancellationToken::default(), &Caller).map(|_| ()).map_err(|d| (d.code, d.target))
}

/// Sets `static_implied` on the stream of `property`, then runs `then` on that stream.
fn flag(property: &'static str, then: impl Fn(&mut Value)) -> impl FnOnce(&mut Value) {
    move |streams: &mut Value| {
        for s in streams.as_array_mut().unwrap().iter_mut().filter(|s| s["property"] == property) {
            s["static_implied"] = json!(true);
            then(s);
        }
    }
}

#[test]
fn folio_accepts_static_implied_only_on_a_keyed_anchor_or_position() {
    with_layer(|run| {
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "anchor", "time": 0, "value": [37.0, 11.0]}));
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "position", "time": 0, "value": [5.0, 6.0]}));
        cmd(run, "property.set_value", json!({"layer_id": L2, "property": "opacity", "value": [40.0]}));
        cmd(run, "keyframe.set", json!({"layer_id": L2, "property": "opacity", "time": 0, "value": [50.0]}));
        assert_eq!((stream(run, "anchor").unwrap()["static_implied"].clone(), stream(run, "position").unwrap()["static_implied"].clone()), (json!(true), json!(true)));
        assert_eq!(verdict(run, |_| {}), Ok(()));
        // An explicit false reads as unset.
        assert_eq!(verdict(run, |streams| streams.as_array_mut().unwrap().iter_mut().for_each(|s| s["static_implied"] = json!(false))), Ok(()));
        let refused = |r: Result<(), (Code, String)>, property: &str| {
            let (code, target) = r.expect_err("must be refused");
            assert_eq!(code, Code::InvalidField);
            assert!(target.ends_with(&format!("/streams/{property}/static_implied")), "{target}");
        };
        // Not on any other property, even a keyed one.
        refused(verdict(run, flag("opacity", |_| {})), "opacity");
        // Not without keyframes: an implied static value is kept only while the property is animated.
        refused(
            verdict(
                run,
                flag("anchor", |s| {
                    s.as_object_mut().unwrap().remove("keyframes");
                }),
            ),
            "anchor",
        );
        refused(verdict(run, flag("position", |s| s["keyframes"] = json!([]))), "position");
    });
}
