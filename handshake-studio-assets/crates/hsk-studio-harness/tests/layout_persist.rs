//! Lane SMB-LAYOUT-PERSIST-1 (STU-LAY-013, STU-LAY-067, STU-SDB-001): a layout edit made through
//! `Viewer::layout_apply` is ONE undo step of the document, named by the edit's command id, and the
//! frames, threads, stories, styles and parent pages it leaves survive save and reopen through the
//! layout editor's load. Oracles: the session history (`edit_json()["history"]`), the reopened layout
//! editor's state, the saved package's entries.
#![cfg(feature = "viewer")]

mod common;

use common::Scratch;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_harness::layout_ui::{LayoutEdit, PageEdit, StyleEdit};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::shell::Panel;
use hsk_studio_package::{Extras, Limits, MemoryAssetSource, Quarantined, WriteOptions, read_package, sha256_hex_of, write_package_with};
use hsk_studio_session::{LAYOUT_RECORDS_ENTRY, LayoutRecords, MAX_LAYOUT_STORIES};
use std::path::{Path, PathBuf};

const DOC: &str = "SDOC-019abcde-0000-7000-8000-00000000c001";

fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
fn page(key: &str) -> StudioPage {
    let m = || len(10.0);
    StudioPage { page_id: key.into(), width: len(200.0), height: len(200.0), side: PageSide::RightHand, margins: LayoutEdges { top: m(), right: m(), bottom: m(), left: m() }, bleed: None, applied_trap_preset: None }
}
/// One spread of two 200 x 200 pages, no layers.
fn document() -> StudioDocument {
    let unassigned = || ProfileBinding::Unassigned(());
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: DOC.into(),
        revision: 1,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: unassigned(),
        working_space_cmyk: unassigned(),
        working_space_gray: unassigned(),
        working_space_spot: unassigned(),
        blending_space: unassigned(),
        artboards: vec![],
        page_spreads: vec![StudioPageSpread {
            schema_id: PageSpreadSchema::V1,
            page_spread_id: "SPGS-019abcde-0000-7000-8000-00000000c001".into(),
            name: "spread".into(),
            parent: Parent::Root(()),
            bounds: Bounds { x: len(0.0), y: len(0.0), width: len(400.0), height: len(200.0) },
            pages: vec![page("page-1"), page("page-2")],
        }],
        layers: vec![],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    }
}

/// The document as a package file, with extra quarantined entries.
fn package(dir: &Path, name: &str, entries: &[Quarantined]) -> PathBuf {
    let token = CancellationToken::default();
    let snapshot = match inspect_bytes(&serde_json::to_vec(&document()).unwrap(), Budget::default(), &token, &Unresolved).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let mut bytes = Vec::new();
    let assets = MemoryAssetSource::new();
    write_package_with(&snapshot, &assets, &Extras { quarantined: entries, previews: &[] }, &WriteOptions::default(), &Limits::default(), &token, &mut bytes).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn open(path: &Path) -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(path).unwrap_or_else(|e| panic!("{e}"));
    v
}

fn undo_labels(v: &Viewer) -> Vec<String> {
    let h = v.edit_json()["history"]["undo"].clone();
    serde_json::from_value(h).unwrap()
}

fn revision(v: &Viewer) -> u64 {
    v.edit_json()["history"]["undo_steps"].as_array().and_then(|s| s.last()).map_or(0, |s| s["revision"].as_u64().unwrap())
}

fn entry_of(path: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).unwrap();
    let opened = read_package(&bytes, &Limits::default(), Budget::default(), &Unresolved, &CancellationToken::default()).unwrap();
    opened.quarantined.iter().find(|q| q.name.starts_with(&format!("studio/{LAYOUT_RECORDS_ENTRY}."))).map(|q| q.bytes.clone())
}

/// Every edit kind the scenario uses commits as one named step; save and reopen give the editor back
/// the same frames, thread, story text, styles and parent pages; undo and a later edit go on from the
/// reopened state.
#[test]
fn layout_edits_are_document_steps_that_survive_save_and_reopen() {
    let scratch = Scratch::new("layout-persist");
    let mut v = open(&package(&scratch.0, "issue.handshake", &[]));
    v.layout_apply(LayoutEdit::DrawFrame { page_id: "page-1".into(), x: 20.0, y: 20.0, w: 120.0, h: 40.0 }).unwrap();
    let a = v.layout_editor().unwrap().selected.clone().unwrap();
    let story = v.layout_editor().unwrap().story_of_frame(&a).unwrap().story_id.clone();
    v.layout_apply(LayoutEdit::ReplaceText { story: story.clone(), start: 0, end: 0, text: "Feature headline\nBody copy".into() }).unwrap();
    v.layout_apply(LayoutEdit::DrawFrame { page_id: "page-2".into(), x: 20.0, y: 20.0, w: 120.0, h: 40.0 }).unwrap();
    let b = v.layout_editor().unwrap().selected.clone().unwrap();
    v.layout_apply(LayoutEdit::Thread { from: a.clone(), to: b.clone() }).unwrap();
    v.layout_apply(LayoutEdit::Page(PageEdit::NewParent { name: "A-Feature".into(), based_on: None })).unwrap();
    v.layout_apply(LayoutEdit::Style(StyleEdit::NewStyle { scope: TypeStyleScope::Paragraph, name: "Body".into(), based_on: None })).unwrap();
    assert_eq!(
        undo_labels(&v),
        ["layout.draw_frame", "layout.replace_text", "layout.draw_frame", "layout.thread", "layout.page.new_parent", "layout.style.new_style"],
        "one named document step per edit"
    );
    assert_eq!(revision(&v), 7, "each edit is one revision");

    // A refused edit changes nothing.
    assert_eq!(v.layout_apply(LayoutEdit::Thread { from: "SLYR-019abcde-0000-7000-8000-0000000fffff".into(), to: b.clone() }), Err("unknown_frame".into()));
    assert_eq!(v.layout_apply(LayoutEdit::DrawFrame { page_id: "page-1".into(), x: f64::NAN, y: 0.0, w: 1.0, h: 1.0 }), Err("invalid_geometry".into()));
    assert_eq!((undo_labels(&v).len(), revision(&v)), (6, 7));

    // Undo takes back the newest edit; the next edit starts from there; undo twice, redo once.
    v.undo().unwrap();
    v.layout_apply(LayoutEdit::Style(StyleEdit::NewStyle { scope: TypeStyleScope::Paragraph, name: "Caption".into(), based_on: None })).unwrap();
    let names: Vec<String> = v.layout_editor().unwrap().doc().styles.styles.iter().map(|s| s.name.clone()).collect();
    assert_eq!(names, ["Caption"], "the undone style is gone, the new one is there");
    v.undo().unwrap();
    v.undo().unwrap();
    v.redo().unwrap();
    v.layout_apply(LayoutEdit::SetColumns { frame: a.clone(), count: 2, gutter: 6.0 }).unwrap();
    assert_eq!(
        undo_labels(&v),
        ["layout.draw_frame", "layout.replace_text", "layout.draw_frame", "layout.thread", "layout.page.new_parent", "layout.set_columns"]
    );
    let before = v.layout_editor().unwrap();
    assert!(before.doc().styles.styles.is_empty());
    assert!(before.records().parents.parents.iter().any(|p| p.name == "A-Feature"), "redo brought the parent page back");
    let (doc, rec) = (before.doc().clone(), before.records().clone());
    let saved = scratch.0.join("issue-saved.handshake");
    v.save(&saved, false).unwrap();
    let stored = entry_of(&saved).expect("the layout records are a package entry");

    let mut w = open(&saved);
    w.toggle_panel(Panel::LayoutEditor).unwrap();
    let ed = w.layout_editor().unwrap();
    assert_eq!(ed.doc().document.layers, doc.document.layers, "frames, columns and threads reopen");
    assert_eq!(ed.doc().document.page_spreads, doc.document.page_spreads);
    assert_eq!((&ed.doc().stories, &ed.doc().styles, &ed.doc().flow), (&doc.stories, &doc.styles, &doc.flow), "stories and styles reopen");
    assert_eq!(ed.records(), &rec, "parent pages and long-document records reopen");
    let state = ed.state_json();
    assert_eq!((&state["frames"][0]["next"], &state["frames"][0]["columns"]), (&serde_json::json!(b), &serde_json::json!(2)));
    assert_eq!(state["stories"][0]["bytes"], "Feature headline
Body copy".len());

    // The reopened document takes edits and undoes them back to the saved bytes.
    w.layout_apply(LayoutEdit::Style(StyleEdit::NewStyle { scope: TypeStyleScope::Character, name: "Num".into(), based_on: None })).unwrap();
    w.undo().unwrap();
    let again = scratch.0.join("issue-again.handshake");
    w.save(&again, false).unwrap();
    assert_eq!(entry_of(&again), Some(stored), "undo restores the records byte for byte");
}

/// An edit applied to the editor directly is not the document's: the next committed edit starts from
/// the session's state.
#[test]
fn a_direct_editor_edit_is_not_committed() {
    let scratch = Scratch::new("layout-direct");
    let mut v = open(&package(&scratch.0, "direct.handshake", &[]));
    v.toggle_panel(Panel::LayoutEditor).unwrap();
    v.layout_editor().unwrap().apply(LayoutEdit::DrawFrame { page_id: "page-1".into(), x: 1.0, y: 1.0, w: 10.0, h: 10.0 }).unwrap();
    v.layout_apply(LayoutEdit::DrawFrame { page_id: "page-2".into(), x: 1.0, y: 1.0, w: 10.0, h: 10.0 }).unwrap();
    let ed = v.layout_editor().unwrap();
    assert_eq!(ed.doc().document.layers.len(), 1, "only the committed frame");
    assert_eq!(ed.doc().document.layers[0].text_frame.as_ref().unwrap().page_id, "page-2");
    assert_eq!(undo_labels(&v), ["layout.draw_frame"]);
}

/// V4-01/02: a hostile entry opens, refuses typed, blocks no open and keeps its bytes on save.
#[test]
fn a_hostile_layout_entry_opens_refuses_typed_and_keeps_its_bytes() {
    let scratch = Scratch::new("layout-hostile");
    let deep = format!("{}{}", "[".repeat(200), "]".repeat(200)).into_bytes();
    let entry = Quarantined { name: format!("studio/{LAYOUT_RECORDS_ENTRY}.{}", sha256_hex_of(&deep)), sha256_hex: sha256_hex_of(&deep), bytes: deep.clone() };
    let mut v = open(&package(&scratch.0, "hostile.handshake", &[entry]));
    assert_eq!(v.layout_apply(LayoutEdit::DrawFrame { page_id: "page-1".into(), x: 1.0, y: 1.0, w: 10.0, h: 10.0 }), Err("layout_records_unreadable".into()));
    assert!(undo_labels(&v).is_empty());
    let saved = scratch.0.join("hostile-saved.handshake");
    v.save(&saved, false).unwrap();
    assert_eq!(entry_of(&saved), Some(deep), "kept byte for byte");
}

/// Review F4 of b21616b7: an entry that decodes but names a page the document does not hold is stale.
/// The next layout edit is refused typed and the records stay byte for byte. The stale entry is built
/// directly as a quarantined package entry naming a missing page (a `page.delete` of a parented page
/// updates the records in its step since lane SMB-R60-FOLLOWUPS-1, so it no longer leaves one).
#[test]
fn a_stale_layout_entry_is_refused_not_replaced() {
    let scratch = Scratch::new("layout-stale");
    let mut v = open(&package(&scratch.0, "stale-source.handshake", &[]));
    v.layout_apply(LayoutEdit::Page(PageEdit::NewParent { name: "A-Feature".into(), based_on: None })).unwrap();
    let parent = v.layout_editor().unwrap().records().parents.parents[0].parent_id.clone();
    v.layout_apply(LayoutEdit::Page(PageEdit::ApplyParent { pages: vec!["page-2".into()], parent: Some(parent) })).unwrap();
    let held = scratch.0.join("stale-held.handshake");
    v.save(&held, false).unwrap();
    let valid = String::from_utf8(entry_of(&held).unwrap()).unwrap();
    assert_eq!(valid.matches("\"page-2\"").count(), 1, "one binding names page-2: {valid}");
    let stale = valid.replace("\"page-2\"", "\"page-9\"").into_bytes();
    let entry = Quarantined { name: format!("studio/{LAYOUT_RECORDS_ENTRY}.{}", sha256_hex_of(&stale)), sha256_hex: sha256_hex_of(&stale), bytes: stale.clone() };

    let mut w = open(&package(&scratch.0, "stale.handshake", &[entry]));
    assert_eq!(w.layout_apply(LayoutEdit::DrawFrame { page_id: "page-1".into(), x: 1.0, y: 1.0, w: 10.0, h: 10.0 }), Err("layout_records_stale".into()));
    assert!(undo_labels(&w).is_empty(), "a refused edit is no step");
    let after = scratch.0.join("stale-after.handshake");
    w.save(&after, false).unwrap();
    assert_eq!(entry_of(&after), Some(stale), "the records are kept, not replaced");
}

/// Review F3 of b21616b7: a write is bounded like every read (the Folio input budget), so no layout
/// edit leaves an entry that no later read takes.
#[test]
fn a_layout_edit_that_would_outgrow_the_entry_budget_is_refused() {
    let scratch = Scratch::new("layout-budget");
    let mut v = open(&package(&scratch.0, "budget.handshake", &[]));
    let text = "a".repeat(220_000);
    let (mut refused, mut last) = (None, String::new());
    for n in 0..5u32 {
        let y = 20.0 + f64::from(n) * 30.0;
        v.layout_apply(LayoutEdit::DrawFrame { page_id: "page-1".into(), x: 20.0, y, w: 120.0, h: 20.0 }).unwrap();
        let frame = v.layout_editor().unwrap().selected.clone().unwrap();
        let story = v.layout_editor().unwrap().story_of_frame(&frame).unwrap().story_id.clone();
        last = frame;
        if let Err(code) = v.layout_apply(LayoutEdit::ReplaceText { story, start: 0, end: 0, text: text.clone() }) {
            refused = Some((n, code));
            break;
        }
    }
    assert_eq!(refused, Some((4, "budget_exceeded".into())), "the fifth story would take the entry past 1 MiB");
    let labels = undo_labels(&v);
    assert_eq!(labels.last().map(String::as_str), Some("layout.draw_frame"), "the refused edit is no step");
    v.layout_apply(LayoutEdit::SetColumns { frame: last, count: 2, gutter: 6.0 }).unwrap();
}

/// V4-02: the story count is checked before any story is read; depth and size before any parse; the
/// records bind to their document.
#[test]
fn layout_records_decode_is_bounded_and_bound_to_the_document() {
    let token = CancellationToken::default();
    let budget = Budget::default();
    let code = |bytes: &[u8]| LayoutRecords::decode(bytes, budget, &token).map(drop).map_err(|d| d.code);
    let records = LayoutRecords::new(DOC);
    let good = records.to_bytes().unwrap();
    assert_eq!(code(&good), Ok(()));
    let mut v: serde_json::Value = serde_json::from_slice(&good).unwrap();
    v["stories"] = serde_json::Value::Array(vec![serde_json::json!({}); MAX_LAYOUT_STORIES + 1]);
    assert_eq!(code(&serde_json::to_vec(&v).unwrap()), Err(Code::BudgetExceeded), "aggregate over budget");
    v["stories"] = serde_json::json!([]);
    v["extra"] = serde_json::json!(1);
    assert_eq!(code(&serde_json::to_vec(&v).unwrap()), Err(Code::UnknownField), "unknown field");
    assert_eq!(code(format!("{}{}", "[".repeat(100), "]".repeat(100)).as_bytes()), Err(Code::BudgetExceeded), "depth");
    assert_eq!(code(&vec![b' '; budget.input_bytes + 1]), Err(Code::BudgetExceeded), "size");
    assert_eq!(code(b"{\"schema_id\":\"hsk.studio.layout_records@2\"}"), Err(Code::InvalidJson), "schema");
    let mut other = document();
    other.document_id = "SDOC-019abcde-0000-7000-8000-00000000c0ff".into();
    assert_eq!(records.validate(&other, budget, &token).map_err(|d| d.code), Err(Code::InvalidField), "another document");
    assert_eq!(records.validate(&document(), budget, &token).map_err(|d| d.code), Ok(()));
}
