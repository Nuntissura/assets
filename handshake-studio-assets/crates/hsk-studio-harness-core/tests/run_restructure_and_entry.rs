//! Lane SMB-AI-AREA-TYPE-3: `Run::restructure_and_entry` commits a structure edit and a session
//! document entry as ONE undo step (the viewer replaces a text layer by a vector layer whose records
//! live in the `vector` entry). Fixture: the harness crate's `tests/fixtures/demo.handshake`; the
//! structure edit creates a new raster layer at the top of the root stack.

use hsk_studio_accord::CancellationToken;
use hsk_studio_harness_core::{Harness, Place, PlacementWrite, Selection, default_actor};
use serde_json::json;

const FIXTURE: &[u8] = include_bytes!("../../hsk-studio-harness/tests/fixtures/demo.handshake");
const NEW: &str = "SLYR-019abcde-0000-7000-8000-0000000000aa";
const TOKEN: &str = "text";

fn harness() -> Harness {
    Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap()
}

/// A new raster layer at the top of the root stack (record and graph source operation).
fn create() -> PlacementWrite {
    let layer = serde_json::from_value(json!({
        "schema_id": "hsk.studio.layer@1", "layer_id": NEW, "name": "new", "parent": null,
        "kind": "raster", "payload": {"payload_kind": "raster", "tiles": []}, "visible": true,
    }))
    .unwrap();
    let operation = serde_json::from_value(json!({"layer_id": NEW, "operation_key": "source", "operation_kind": "source", "inputs": [],
        "outputs": [{"port_key": "out", "value_type": "image", "cardinality": "one", "semantic": "source"}]}))
    .unwrap();
    PlacementWrite { layer_id: NEW.to_string(), to: Some(Place::AtBeginning(None)), create: Some((layer, operation)) }
}

fn has_layer(run: &hsk_studio_harness_core::Run<'_>, id: &str) -> bool {
    run.snapshot().unwrap().document().layers.iter().any(|l| l.layer_id == id)
}

#[test]
fn structure_and_entry_undo_and_redo_as_one_step() {
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(default_actor(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    assert!(run.document_entry(TOKEN).is_none());
    let ok = run.restructure_and_entry("replace", &[create()], TOKEN, b"{\"v\":1}".to_vec());
    assert!(ok, "{:?}", run.ops().last());
    assert!(has_layer(&run, NEW));
    assert_eq!(run.document_entry(TOKEN), Some(&b"{\"v\":1}"[..]));
    assert_eq!(run.history()["undo"].as_array().unwrap().len(), 1, "{}", run.history());
    // One undo restores both the layer and the (absent) entry; one redo applies both again.
    assert!(run.undo());
    assert!(!has_layer(&run, NEW));
    assert!(run.document_entry(TOKEN).is_none());
    assert!(run.history()["undo"].as_array().unwrap().is_empty());
    assert!(run.redo());
    assert!(has_layer(&run, NEW));
    assert_eq!(run.document_entry(TOKEN), Some(&b"{\"v\":1}"[..]));
}

#[test]
fn refused_structure_or_entry_changes_nothing() {
    let h = harness();
    let cancel = CancellationToken::default();
    let mut run = h.run(default_actor(), &cancel);
    assert!(run.open_bytes(FIXTURE));
    // No writes: the structure edit is refused, the entry is not written.
    assert!(!run.restructure_and_entry("replace", &[], TOKEN, b"x".to_vec()));
    assert!(run.document_entry(TOKEN).is_none());
    // An unknown entry token: the structure edit is undone, nothing stays on the undo stack.
    assert!(!run.restructure_and_entry("replace", &[create()], "not-a-token", b"x".to_vec()));
    assert!(!has_layer(&run, NEW));
    assert!(run.history()["undo"].as_array().unwrap().is_empty(), "{}", run.history());
    // Not open: refused.
    let mut closed = h.run(default_actor(), &cancel);
    assert!(!closed.restructure_and_entry("replace", &[], TOKEN, b"x".to_vec()));
}
