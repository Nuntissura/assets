//! Lane SMB-AI-AREA-TYPE-3: `Run::restructure_and_entry` commits a structure edit and a session
//! document entry as ONE undo step (the viewer replaces a text layer by a vector layer whose records
//! live in the `vector` entry). Fixture: the harness crate's `tests/fixtures/demo.handshake`
//! (group layer ...0001, raster layer ...0002, both stacked at the root).

use hsk_studio_accord::CancellationToken;
use hsk_studio_harness_core::{Harness, PlacementWrite, Selection, default_actor};

const FIXTURE: &[u8] = include_bytes!("../../hsk-studio-harness/tests/fixtures/demo.handshake");
const L2: &str = "SLYR-019abcde-0000-7000-8000-000000000002";
const TOKEN: &str = "text";

fn harness() -> Harness {
    Harness::new(Selection { providers: vec!["chronicle.patch".to_string()], ..Selection::default() }).unwrap()
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
    let delete = PlacementWrite { layer_id: L2.to_string(), to: None, create: None };
    assert!(run.restructure_and_entry("replace", &[delete], TOKEN, b"{\"v\":1}".to_vec()));
    assert!(!has_layer(&run, L2));
    assert_eq!(run.document_entry(TOKEN), Some(&b"{\"v\":1}"[..]));
    assert_eq!(run.history()["undo"].as_array().unwrap().len(), 1, "{}", run.history());
    // One undo restores both the layer and the (absent) entry; one redo applies both again.
    assert!(run.undo());
    assert!(has_layer(&run, L2));
    assert!(run.document_entry(TOKEN).is_none());
    assert!(run.history()["undo"].as_array().unwrap().is_empty());
    assert!(run.redo());
    assert!(!has_layer(&run, L2));
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
    let delete = PlacementWrite { layer_id: L2.to_string(), to: None, create: None };
    assert!(!run.restructure_and_entry("replace", &[delete], "not-a-token", b"x".to_vec()));
    assert!(has_layer(&run, L2));
    assert!(run.history()["undo"].as_array().unwrap().is_empty(), "{}", run.history());
    // Not open: refused.
    let mut closed = h.run(default_actor(), &cancel);
    assert!(!closed.restructure_and_entry("replace", &[], TOKEN, b"x".to_vec()));
}
