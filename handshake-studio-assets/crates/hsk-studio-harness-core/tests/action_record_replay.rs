//! Lane SMB-PS-ACTIONS-1 slice 2: recording the `Run` command stream and replaying it as ONE
//! transactional unit and ONE undo step ([STU-AUT-109], [STU-AUT-004], [STU-AUT-100]).
//! Fixture: the harness crate's `tests/fixtures/demo.handshake` (group layer ...0001
//! "renamed-by-session" visible, raster layer ...0002 "layer-2" hidden; layers not stacked).

use hsk_studio_accord::CancellationToken;
use hsk_studio_harness_core::action::{self, ActionSet, DialogOption, UnitToken};
use hsk_studio_harness_core::{ActionRecord, Harness, Place, PlacementWrite, PlayMode, RecordedValue, Run, Selection};
mod support;
use support::test_context;
use serde_json::{Value, json};

const FIXTURE: &[u8] = include_bytes!("../../hsk-studio-harness/tests/fixtures/demo.handshake");
const L1: &str = "SLYR-019abcde-0000-7000-8000-000000000001";
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";
const MISSING: &str = "SLYR-019abcde-0000-7000-8000-0000000000ff";
const ACT: &str = "SACT-019abcde-0000-7000-8000-0000000000a1";
const RECORDED: [&str; 3] = ["rename", "set_opacity", "set_visible"];

fn harness() -> Harness {
    Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap()
}

/// (name, opacity, visible) of a layer of the working document.
fn values(run: &Run<'_>, id: &str) -> (String, f64, bool) {
    let l = run.snapshot().unwrap().document().layers.iter().find(|l| l.layer_id == id).unwrap();
    (l.name.clone(), l.effective_appearance().opacity, l.visible)
}

fn op_names(run: &Run<'_>) -> Vec<String> {
    run.ops().iter().map(|o| o["op"].as_str().unwrap().to_string()).collect()
}

fn undo_labels(run: &Run<'_>) -> Value {
    run.history()["undo"].clone()
}

/// Records rename L1, opacity L2 40 %, show L2. Returns the action and the recording's op names.
fn record_three(h: &Harness, cancel: &CancellationToken) -> (ActionRecord, action::RecordingReport, Vec<String>) {
    let mut run = h.run(test_context(), cancel);
    assert!(run.open_bytes(FIXTURE));
    assert!(run.start_recording(ACT, "tidy"));
    assert!(run.rename(L1, "tidied", None));
    assert!(run.set_opacity(L2, 40.0, None));
    assert!(run.set_visible(L2, true, None));
    let (action, report) = run.stop_recording().expect("recording");
    (action, report, op_names(&run))
}

#[test]
fn records_rename_opacity_and_visible_as_three_typed_steps() {
    let h = harness();
    let cancel = CancellationToken::default();
    let (action, report, ops) = record_three(&h, &cancel);
    assert_eq!(ops, ["open", "action_record_start", "rename", "set_opacity", "set_visible", "action_record_stop"]);
    assert!(action.action_id == ACT && action.name == "tidy" && action.declared_step_count == 3);
    let ids: Vec<&str> = action.steps.iter().map(|s| s.command_id.as_str()).collect();
    assert_eq!(ids, ["layer.rename", "layer.set_opacity", "layer.set_visible"]);
    let p = |i: usize, k: &str| action.steps[i].parameters.as_ref().unwrap()[k].clone();
    assert_eq!(p(0, "layer"), RecordedValue::Reference(L1.into()));
    assert_eq!(p(0, "name"), RecordedValue::String("tidied".into()));
    assert_eq!(p(1, "layer"), RecordedValue::Reference(L2.into()));
    assert_eq!(p(1, "opacity"), RecordedValue::Measured { value: 40.0, unit: Some(UnitToken::Percent) });
    assert_eq!(p(2, "visible"), RecordedValue::Boolean(true));
    assert!(action.steps.iter().enumerate().all(|(i, s)| s.index == i as u64 && s.enabled && s.dialog_options == DialogOption::Silent && s.has_parameters));
    assert!(report.not_recordable.is_empty() && report.refused.is_empty() && report.steps == 3, "{}", report.to_json());
    // The recorded action is a valid portable artifact.
    let set = ActionSet { action_set_id: "recorded".into(), name: "Recorded".into(), actions: vec![action] };
    let imported = ActionSet::import(&set.export()).unwrap();
    assert!(imported.diagnostics.is_empty(), "{:?}", imported.diagnostics);
}

#[test]
fn replay_is_one_undo_step_with_the_same_ops_and_one_undo_reverts_all() {
    let h = harness();
    let cancel = CancellationToken::default();
    let (action, _, recorded_ops) = record_three(&h, &cancel);

    let mut run = h.run(test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    let (l1, l2) = (values(&run, L1), values(&run, L2));
    assert!(run.play_action(&action, PlayMode::Headless), "{:?}", run.ops().last());
    assert!(run.verify_live(None, vec![(L1.into(), "name", json!("tidied")), (L2.into(), "opacity", json!(40.0)), (L2.into(), "visible", json!(true))]), "{:?}", run.ops().last());
    assert_eq!(undo_labels(&run), json!(["action tidy"]), "exactly ONE new undo label");
    // The replayed op records carry the same op names as the originals.
    let replay_ops = op_names(&run);
    let pick = |ops: &[String]| ops.iter().filter(|o| RECORDED.contains(&o.as_str())).cloned().collect::<Vec<_>>();
    assert_eq!(pick(&replay_ops), pick(&recorded_ops));
    assert_eq!(&replay_ops[..5], ["open", "rename", "set_opacity", "set_visible", "action_play"]);
    let play = &run.ops()[4];
    assert!(play["ok"] == true && play["detail"]["steps_played"] == 3 && play["detail"]["history_label"] == "action tidy", "{play}");

    // One undo reverts all three, one redo re-applies all three.
    assert!(run.undo(), "{:?}", run.ops().last());
    assert_eq!((values(&run, L1), values(&run, L2)), (l1.clone(), l2.clone()));
    assert_eq!(undo_labels(&run), json!([]));
    assert_eq!(run.history()["redo"], json!(["action tidy"]));
    assert!(run.redo(), "{:?}", run.ops().last());
    assert_eq!(values(&run, L1).0, "tidied");
    assert_eq!((values(&run, L2).1, values(&run, L2).2), (40.0, true));
    assert_eq!(undo_labels(&run), json!(["action tidy"]));
    assert!(run.undo());
    assert_eq!((values(&run, L1), values(&run, L2)), (l1, l2));
    // Every frame law still holds.
    let report = run.finish("test");
    assert_eq!(report["observe"]["one_frame_per_operation"], true, "{report}");
}

#[test]
fn a_failing_step_leaves_content_and_history_unchanged() {
    let h = harness();
    let cancel = CancellationToken::default();
    let (mut action, _, _) = record_three(&h, &cancel);
    // Step 2 renames a layer the document does not have: steps 0 and 1 must be rolled back.
    let mut bad = action.steps[0].clone();
    bad.parameters.as_mut().unwrap().insert("layer".into(), RecordedValue::Reference(MISSING.into()));
    action.steps.insert(2, bad);
    for (i, s) in action.steps.iter_mut().enumerate() {
        s.index = i as u64;
    }
    action.declared_step_count = 4;

    let mut run = h.run(test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    // A redo entry the replay must keep.
    assert!(run.rename(L2, "before", None) && run.undo());
    let history = run.history();
    let (l1, l2) = (values(&run, L1), values(&run, L2));
    assert!(!run.play_action(&action, PlayMode::Headless));
    let last = run.ops().last().unwrap().clone();
    assert!(last["op"] == "action_play" && last["error"] == "action_step_reference_unresolved", "{last}");
    assert!(last["detail"]["step_index"] == 2 && last["detail"]["command_id"] == "layer.rename" && last["detail"]["rolled_back"] == true, "{last}");
    assert_eq!(last["detail"]["detail"]["reference"], MISSING);
    assert_eq!((values(&run, L1), values(&run, L2)), (l1.clone(), l2.clone()), "content unchanged");
    assert_eq!(run.history(), history, "history unchanged");

    // An unresolvable command refuses before any step runs: one record, no revision change.
    let mut unknown = action.clone();
    unknown.steps[3].command_id = "layer.no_such_command".into();
    unknown.steps.remove(2);
    let (ops, revision) = (run.ops().len(), run.snapshot().unwrap().document().revision);
    assert!(!run.play_action(&unknown, PlayMode::Headless));
    assert_eq!(run.ops().len(), ops + 1);
    assert_eq!(run.snapshot().unwrap().document().revision, revision);
    let last = run.ops().last().unwrap();
    assert!(last["error"] == "action_step_command_unresolvable" && last["detail"]["step_index"] == 3 && last["detail"]["command_id"] == "layer.no_such_command", "{last}");
    // A wrongly typed parameter is refused the same way, naming the key.
    let mut typed = action.clone();
    typed.steps.remove(2);
    typed.steps[1].parameters.as_mut().unwrap().insert("opacity".into(), RecordedValue::Number(40.0));
    assert!(!run.play_action(&typed, PlayMode::Headless));
    let last = run.ops().last().unwrap();
    assert!(last["error"] == "action_step_parameter_invalid" && last["detail"]["key"] == "opacity", "{last}");
    assert_eq!(run.history(), history);
}

#[test]
fn a_disabled_step_is_skipped_and_kept() {
    let h = harness();
    let cancel = CancellationToken::default();
    let (mut action, _, _) = record_three(&h, &cancel);
    action.steps[0].enabled = false;
    let mut run = h.run(test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    let l1 = values(&run, L1);
    assert!(run.play_action(&action, PlayMode::Headless), "{:?}", run.ops().last());
    assert_eq!(values(&run, L1), l1, "the disabled rename did not run");
    assert_eq!((values(&run, L2).1, values(&run, L2).2), (40.0, true));
    let play = run.ops().last().unwrap();
    assert!(play["detail"]["skipped"] == json!([0]) && play["detail"]["steps_played"] == 2, "{play}");
    assert_eq!(action.steps.len(), 3, "the record keeps the disabled step");
    assert!(!op_names(&run).contains(&"rename".to_string()));
}

#[test]
fn dialog_policy_in_headless_and_interactive_playback() {
    let h = harness();
    let cancel = CancellationToken::default();
    let (mut action, _, _) = record_three(&h, &cancel);
    action.steps[1].dialog_options = DialogOption::AlwaysShow;
    action.steps[2].dialog_options = DialogOption::ShowInInteractivePlayback;
    let mut run = h.run(test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    let revision = run.snapshot().unwrap().document().revision;
    assert!(!run.play_action(&action, PlayMode::Headless));
    let last = run.ops().last().unwrap();
    assert!(last["error"] == "action_always_show_dialog_in_headless" && last["detail"]["step_index"] == 1 && last["detail"]["action_id"] == ACT, "{last}");
    assert_eq!(run.snapshot().unwrap().document().revision, revision, "refused before any step ran");
    // Interactive: plays; the engine has no dialog host, so the steps that would show one are listed.
    assert!(run.play_action(&action, PlayMode::Interactive), "{:?}", run.ops().last());
    assert_eq!(run.ops().last().unwrap()["detail"]["dialogs_not_shown"], json!([1, 2]));
    // Headless with show_in_interactive_playback only: suppressed, plays.
    action.steps[1].dialog_options = DialogOption::Silent;
    assert!(run.undo());
    assert!(run.play_action(&action, PlayMode::Headless));
    assert_eq!(run.ops().last().unwrap()["detail"]["dialogs_not_shown"], json!([]));
    println!("NOT_PROVEN: parameter dialogs during interactive playback (no GUI dialog host; dialogs_not_shown lists them)");
}

#[test]
fn non_recordable_operations_are_reported_never_silently_dropped() {
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    assert!(!run.start_recording("SACT-bad", "x"));
    assert_eq!(run.ops().last().unwrap()["error"], "action_id_malformed");
    assert!(run.start_recording(ACT, "mixed"));
    assert!(!run.start_recording(ACT, "again"));
    assert!(run.summary());
    assert!(run.panel(L1));
    assert!(run.rename(L1, "kept", None));
    assert!(!run.rename(MISSING, "nope", None), "refused: not recorded");
    assert!(run.checkpoint().is_some());
    assert!(run.undo());
    assert!(run.set_document_entry("text", Some(b"{}".to_vec())).is_ok());
    // A restructure that creates a layer carries a record payload: not recordable.
    let new = "SLYR-019abcde-0000-7000-8000-0000000000aa";
    let layer = serde_json::from_value(json!({"schema_id": "hsk.studio.layer@1", "layer_id": new, "name": "new", "parent": null, "kind": "raster", "payload": {"payload_kind": "raster", "tiles": []}, "visible": true})).unwrap();
    let operation = serde_json::from_value(json!({"layer_id": new, "operation_key": "source", "operation_kind": "source", "inputs": [], "outputs": [{"port_key": "out", "value_type": "image", "cardinality": "one", "semantic": "source"}]})).unwrap();
    assert!(run.restructure("new_layer", &[PlacementWrite { layer_id: new.into(), to: Some(Place::AtBeginning(None)), create: Some((layer, operation)) }]));
    let (action, report) = run.stop_recording().unwrap();
    assert!(run.stop_recording().is_none());
    assert_eq!(action.steps.len(), 1);
    assert_eq!(action.steps[0].command_id, "layer.rename");
    let nr: Vec<(&str, &str)> = report.not_recordable.iter().map(|n| (n.command_id, n.reason)).collect();
    assert_eq!(
        nr,
        [
            ("document.summary", "recordable_false"),
            ("layer.panel", "recordable_false"),
            ("document.checkpoint", "recordable_false"),
            ("history.undo", "recordable_false"),
            ("document.set_document_entry", "recordable_false"),
            ("layer.restructure", "restructure_create_payload"),
        ]
    );
    assert_eq!(report.refused, vec![("layer.rename".to_string(), "unknown_layer".to_string())]);
    for n in &report.not_recordable {
        assert!(!action::command(n.command_id).unwrap().recordable || n.reason != "recordable_false");
    }
    println!("NOT_PROVEN: Photoshop behaviour for refused commands during recording (Studio reading: not recorded, reported as refused; UNVERIFIED (D5))");
    println!("NOT_PROVEN: byte-payload steps (set_tiles, vector paths, document entries, sequences) as STU-AUT-111 path_literal / tool_stroke step kinds");
    println!("NOT_PROVEN: chronicle-level single transaction for a replay (each step is its own revision; atomicity is rollback, one grouped undo step)");
}
