//! Lane SMB-R60-FOLLOWUPS-1 item 6 (feature `viewer`; validation round R60 attempt 2 probes b9 and i3):
//! `page.delete` of a page a parent page is applied to updates the stored layout records in the same undo
//! step, so the layout stays editable after save and reopen and undo restores the page and its binding
//! together; `layout.open`, which commits a sized open as an undo step, declares it mutates (STU-AUT-147).
//! Oracles: the reopened layout editor, the session history, the document's pages.
#![cfg(feature = "viewer")]

mod common;

use common::Scratch;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_harness::layout_ui::{LayoutEdit, PageEdit};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness_core::action;
use hsk_studio_package::{Extras, Limits, MemoryAssetSource, WriteOptions, write_package_with};
use std::path::{Path, PathBuf};

const DOC: &str = "SDOC-019abcde-0000-7000-8000-00000000c060";

fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
fn page(key: &str) -> StudioPage {
    let m = || len(10.0);
    StudioPage { page_id: key.into(), width: len(200.0), height: len(200.0), side: PageSide::RightHand, margins: LayoutEdges { top: m(), right: m(), bottom: m(), left: m() }, bleed: None, applied_trap_preset: None }
}
/// One spread of three 200 x 200 pages, no layers.
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
            page_spread_id: "SPGS-019abcde-0000-7000-8000-00000000c060".into(),
            name: "spread".into(),
            parent: Parent::Root(()),
            bounds: Bounds { x: len(0.0), y: len(0.0), width: len(600.0), height: len(200.0) },
            pages: vec![page("page-1"), page("page-2"), page("page-3")],
        }],
        layers: vec![],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    }
}

fn package(dir: &Path, name: &str) -> PathBuf {
    let token = CancellationToken::default();
    let snapshot = match inspect_bytes(&serde_json::to_vec(&document()).unwrap(), Budget::default(), &token, &Unresolved).unwrap() {
        Inspection::Editable(s) => s,
        Inspection::ReadOnly(_) => panic!("read-only fixture"),
    };
    let mut bytes = Vec::new();
    let assets = MemoryAssetSource::new();
    write_package_with(&snapshot, &assets, &Extras { quarantined: &[], previews: &[] }, &WriteOptions::default(), &Limits::default(), &token, &mut bytes).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn open(path: &Path) -> Viewer {
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(path).unwrap_or_else(|e| panic!("{e}"));
    v
}

fn pages(v: &Viewer) -> Vec<String> {
    v.run().snapshot().unwrap().document().page_spreads.iter().flat_map(|s| &s.pages).map(|p| p.page_id.clone()).collect()
}

fn bound_pages(v: &mut Viewer) -> Vec<String> {
    v.layout_sync();
    v.layout_editor().unwrap().records().parents.page_bindings.iter().map(|b| b.page_id.clone()).collect()
}

fn frame_on_page_1() -> LayoutEdit {
    LayoutEdit::DrawFrame { page_id: "page-1".into(), x: 1.0, y: 1.0, w: 10.0, h: 10.0 }
}

/// A parent page applied to page-2 and page-3; returns the viewer.
fn parented(scratch: &Scratch) -> Viewer {
    let mut v = open(&package(&scratch.0, "parented.handshake"));
    v.layout_apply(LayoutEdit::Page(PageEdit::NewParent { name: "A-Feature".into(), based_on: None })).unwrap();
    let parent = v.layout_editor().unwrap().records().parents.parents[0].parent_id.clone();
    v.layout_apply(LayoutEdit::Page(PageEdit::ApplyParent { pages: vec!["page-2".into(), "page-3".into()], parent: Some(parent) })).unwrap();
    v
}

/// Probe b9: delete a page with a parent page, save, reopen: the layout is still editable, the other
/// page keeps its binding.
#[test]
fn r60_b9_a_deleted_parented_page_leaves_the_layout_editable_after_reopen() {
    let scratch = Scratch::new("r60-b9-reopen");
    let mut v = parented(&scratch);
    v.delete_page("page-2").unwrap();
    assert_eq!(pages(&v), ["page-1", "page-3"]);
    v.layout_apply(frame_on_page_1()).expect("the layout is editable after the delete");
    let saved = scratch.0.join("deleted.handshake");
    v.save(&saved, false).unwrap();
    let mut back = open(&saved);
    assert_eq!(bound_pages(&mut back), ["page-3"], "the deleted page's binding is gone, the other kept");
    back.layout_apply(frame_on_page_1()).expect("editable after reopen");
    back.layout_apply(LayoutEdit::Page(PageEdit::ApplyParent { pages: vec!["page-1".into()], parent: None })).expect("page edits go on after reopen");
}

/// Probe b9: the delete is ONE undo step for the page and its records: undo restores both, redo removes
/// both again.
#[test]
fn r60_b9_undo_restores_the_page_and_its_binding_together() {
    let scratch = Scratch::new("r60-b9-undo");
    let mut v = parented(&scratch);
    let depth = v.edit_json()["history"]["undo"].as_array().unwrap().len();
    v.delete_page("page-2").unwrap();
    let undo = v.edit_json()["history"]["undo"].as_array().unwrap().clone();
    assert_eq!(undo.len(), depth + 1, "one step: {undo:?}");
    assert_eq!(bound_pages(&mut v), ["page-3"]);
    v.undo().unwrap();
    assert_eq!(pages(&v), ["page-1", "page-2", "page-3"]);
    assert_eq!(bound_pages(&mut v), ["page-2", "page-3"], "the binding comes back with the page");
    v.redo().unwrap();
    assert_eq!(pages(&v), ["page-1", "page-3"]);
    assert_eq!(bound_pages(&mut v), ["page-3"]);
    v.layout_apply(frame_on_page_1()).expect("editable after redo");
}

/// Item 6: a page delete never leaves a stale entry. A page a layout text frame sits on is refused typed
/// `page_not_empty` with nothing changed, and the layout stays editable, also after save and reopen.
#[test]
fn r60_b9_a_page_delete_never_leaves_the_layout_locked() {
    let scratch = Scratch::new("r60-b9-frame");
    let mut v = parented(&scratch);
    v.layout_apply(LayoutEdit::DrawFrame { page_id: "page-2".into(), x: 1.0, y: 1.0, w: 10.0, h: 10.0 }).unwrap();
    let depth = v.edit_json()["history"]["undo"].as_array().unwrap().len();
    let e = v.delete_page("page-2").expect_err("a text frame sits on page-2");
    assert!(e.contains("page_not_empty"), "a typed refusal: {e}");
    assert_eq!(pages(&v), ["page-1", "page-2", "page-3"], "a refusal changes nothing");
    assert_eq!(v.edit_json()["history"]["undo"].as_array().unwrap().len(), depth, "and adds no step");
    v.layout_apply(frame_on_page_1()).expect("the layout stays editable");
    let saved = scratch.0.join("frame.handshake");
    v.save(&saved, false).unwrap();
    let mut back = open(&saved);
    back.layout_apply(frame_on_page_1()).expect("editable after reopen");
}

/// Probe i3: a sized `layout.open` on an open document adds an undo step, and its row says it mutates.
#[test]
fn r60_i3_layout_open_that_adds_an_undo_step_declares_it_mutates() {
    let entry = action::command("layout.open").expect("layout.open row");
    assert!(entry.mutates_authority, "layout.open commits a sized open as a document step");
    let mut v = Viewer::new(Prefer::Cpu);
    v.new_document(&hsk_studio_harness_core::document_new::NewDocument::new(hsk_studio_harness_core::document_new::DocumentProfile::Vector, Some((128.0, 128.0))), None).unwrap();
    let before = v.edit_json()["history"]["undo"].as_array().unwrap().len();
    v.layout_open(Some((200.0, 200.0)), Some(1)).unwrap();
    assert_eq!(v.edit_json()["history"]["undo"].as_array().unwrap().len(), before + 1, "a sized open is a step");
}
