//! Lane SMB-BOARD-1 (review F3 of e4d033aa, lead ruling 2026-11-13): `Run::catalog_step` names a command made of
//! several public steps by its catalog id: ONE group step labelled with the id, a closing record whose op and
//! command id are the id and whose actor is the caller; a failure inside rolls every inner step back with no
//! redo entry; one undo and one redo of the group are exact; a nested catalog step is refused. Fixture: the
//! harness crate's `tests/fixtures/demo.handshake`.

use hsk_studio_accord::CancellationToken;
use hsk_studio_harness_core::{Harness, Place, PlacementWrite, Run, Selection};
mod support;
use serde_json::{Value, json};
use support::test_context;

const FIXTURE: &[u8] = include_bytes!("../../hsk-studio-harness/tests/fixtures/demo.handshake");
const A: &str = "SLYR-019abcde-0000-7000-8000-0000000000a1";
const B: &str = "SLYR-019abcde-0000-7000-8000-0000000000a2";
const ID: &str = "board.sticky.create";

fn harness() -> Harness {
    Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap()
}

/// A new raster layer `id` at the top of the root stack.
fn create(id: &str) -> PlacementWrite {
    let layer = serde_json::from_value(json!({
        "schema_id": "hsk.studio.layer@1", "layer_id": id, "name": "new", "parent": null,
        "kind": "raster", "payload": {"payload_kind": "raster", "tiles": []}, "visible": true,
    }))
    .unwrap();
    let operation = serde_json::from_value(json!({"layer_id": id, "operation_key": "source", "operation_kind": "source", "inputs": [],
        "outputs": [{"port_key": "out", "value_type": "image", "cardinality": "one", "semantic": "source"}]}))
    .unwrap();
    PlacementWrite { layer_id: id.to_string(), to: Some(Place::AtBeginning(None)), create: Some((layer, operation)) }
}

fn has(run: &Run<'_>, id: &str) -> bool {
    run.snapshot().unwrap().document().layers.iter().any(|l| l.layer_id == id)
}

fn labels(run: &Run<'_>, key: &str) -> Vec<String> {
    run.history()[key].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_owned()).collect()
}

/// The layers and the two entries the steps write.
fn state(run: &Run<'_>) -> (bool, bool, Option<Vec<u8>>, Option<Vec<u8>>) {
    (has(run, A), has(run, B), run.document_entry("vector").map(<[u8]>::to_vec), run.document_entry("text").map(<[u8]>::to_vec))
}

/// Two structure steps and an entry step inside one catalog step.
fn two_layers(run: &mut Run<'_>) -> bool {
    run.restructure_and_entry(ID, &[create(A)], "vector", b"{\"v\":1}".to_vec()) && run.restructure_and_entry(ID, &[create(B)], "text", b"{\"t\":1}".to_vec())
}

#[test]
fn the_group_label_and_the_closing_record_are_the_catalog_id_and_one_undo_and_redo_are_exact() {
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    let before = state(&run);
    assert!(run.catalog_step(ID, two_layers), "{:?}", run.ops().last());
    let after = state(&run);
    assert_eq!(after, (true, true, Some(b"{\"v\":1}".to_vec()), Some(b"{\"t\":1}".to_vec())));
    assert_eq!(labels(&run, "undo"), vec![ID.to_owned()], "{}", run.history());
    let record = run.ops().last().cloned().unwrap_or(Value::Null);
    assert_eq!((record["op"].as_str(), record["command_id"].as_str(), record["ok"].as_bool()), (Some(ID), Some(ID), Some(true)), "{record}");
    assert_eq!(record["actor"]["account_id"], "account", "{record}");
    assert_eq!(record["detail"]["steps"], 2, "{record}");
    // One undo and one redo of the group are exact.
    assert!(run.undo());
    assert_eq!(state(&run), before);
    assert!(labels(&run, "undo").is_empty());
    assert_eq!(labels(&run, "redo"), vec![ID.to_owned()], "{}", run.history());
    assert!(run.redo());
    assert_eq!(state(&run), after);
    assert_eq!(labels(&run, "undo"), vec![ID.to_owned()]);
    assert!(labels(&run, "redo").is_empty());
}

#[test]
fn a_failure_inside_rolls_every_inner_step_back_and_leaves_no_redo_entry() {
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    // A redo entry from before stays as it was.
    assert!(run.restructure_and_entry("earlier", &[create(B)], "text", b"{\"e\":1}".to_vec()));
    assert!(run.undo());
    let redo = labels(&run, "redo");
    assert_eq!(redo.len(), 1);
    let before = state(&run);
    let ok = run.catalog_step(ID, |run| run.restructure_and_entry(ID, &[create(A)], "vector", b"{\"v\":1}".to_vec()) && run.restructure_and_entry(ID, &[create(B)], "not-a-token", b"x".to_vec()));
    assert!(!ok);
    assert_eq!(state(&run), before, "nothing stays applied");
    assert!(labels(&run, "undo").is_empty(), "{}", run.history());
    assert_eq!(labels(&run, "redo"), redo, "the rollback leaves no redo entry");
    let record = run.ops().last().cloned().unwrap_or(Value::Null);
    assert_eq!((record["op"].as_str(), record["command_id"].as_str(), record["error"].as_str()), (Some(ID), Some(ID), Some("catalog_step_failed")), "{record}");
}

#[test]
fn a_nested_catalog_step_is_refused_and_the_outer_one_rolls_back() {
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    let before = state(&run);
    let mut inner_ran = false;
    let ok = run.catalog_step(ID, |run| run.restructure_and_entry(ID, &[create(A)], "vector", b"{\"v\":1}".to_vec()) && run.catalog_step("board.node.move", |_| {
        inner_ran = true;
        true
    }));
    assert!(!ok);
    assert!(!inner_ran, "the nested step's work never runs");
    let refused = run.ops().iter().rev().find(|r| r["op"] == "board.node.move").cloned().unwrap_or(Value::Null);
    assert_eq!(refused["error"], "catalog_step_nested", "{refused}");
    assert_eq!(state(&run), before);
    assert!(labels(&run, "undo").is_empty());
    assert!(labels(&run, "redo").is_empty());
}

#[test]
fn a_catalog_step_that_changes_nothing_writes_its_record_and_keeps_the_redo_history() {
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(test_context(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    assert!(run.restructure_and_entry("earlier", &[create(B)], "text", b"{\"e\":1}".to_vec()));
    assert!(run.undo());
    assert!(run.catalog_step(ID, |_| true));
    assert_eq!(labels(&run, "redo").len(), 1);
    assert!(labels(&run, "undo").is_empty());
    let record = run.ops().last().cloned().unwrap_or(Value::Null);
    assert_eq!((record["op"].as_str(), record["detail"]["steps"].as_u64()), (Some(ID), Some(0)), "{record}");
}
