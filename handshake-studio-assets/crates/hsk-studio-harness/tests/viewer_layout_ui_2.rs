//! Lane SMB-LAYOUT-UI-2 slice 1: pages and parent-pages panel, headless (fixture in
//! `layout_ui_fixture`). InDesign page/parent behaviour UNVERIFIED (D5).
#![cfg(feature = "layout-ui")]
mod layout_ui_fixture;
use hsk_studio_folio::*;
use hsk_studio_harness::layout_ui::{LayoutEdit, LayoutEditor, PageEdit, widget};
use layout_ui_fixture::{doc, id, rect_colours, rects_of, ui_frames};

fn page_order(ed: &LayoutEditor) -> Vec<(usize, String)> {
    ed.page_rows().into_iter().map(|r| (r.spread, r.page_id)).collect()
}
fn pe(e: PageEdit) -> LayoutEdit {
    LayoutEdit::Page(e)
}
#[test]
fn pages_and_parent_pages_panel_edits_are_single_undo_steps() {
    let original = doc();
    let mut ed = LayoutEditor::new(original.clone());
    let rec0 = ed.records().clone();

    // Parent "A" with a header frame drawn on page 2, applied to both pages.
    ed.apply(pe(PageEdit::NewParent { name: "A-Parent".into(), based_on: None })).unwrap();
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-2".into(), x: 20.0, y: 150.0, w: 100.0, h: 20.0 }).unwrap();
    let header = ed.selected.clone().unwrap();
    ed.apply(pe(PageEdit::AddParentItem { parent: "P1".into(), frame: header.clone() })).unwrap();
    ed.apply(pe(PageEdit::ApplyParent { pages: vec!["page-1".into(), "page-2".into()], parent: Some("P1".into()) })).unwrap();
    assert_eq!(ed.page_items("page-1").unwrap(), vec![("item-1".to_owned(), "parent:P1".to_owned())]);
    // A parent item is not a page frame: no hit, drawn as an inherited overlay.
    assert_eq!(ed.frame_at("page-2", 30.0, 160.0), None);
    let before_override = (ed.doc().clone(), ed.records().clone());

    // Override = page-local copy (with a copy of its story); remove override restores inheritance.
    ed.apply(pe(PageEdit::OverrideItem { page: "page-1".into(), item: "item-1".into() })).unwrap();
    assert_eq!(ed.page_items("page-1").unwrap(), vec![("item-1".to_owned(), "override:P1".to_owned())]);
    assert_eq!((ed.doc().document.layers.len(), ed.doc().stories.len()), (3, 3));
    let copy = ed.doc().document.layers[2].layer_id.clone();
    assert_eq!(ed.frame_at("page-1", 30.0, 160.0), Some(copy.clone()), "the override copy is a page frame");
    assert_eq!(ed.apply(pe(PageEdit::OverrideItem { page: "page-1".into(), item: "item-1".into() })).unwrap_err().code(), "already_overridden");
    ed.apply(pe(PageEdit::RemoveOverride { page: "page-1".into(), item: "item-1".into() })).unwrap();
    assert_eq!((ed.doc(), ed.records()), (&before_override.0, &before_override.1));
    // STU-LAY-007 "cannot unlink an item that was not overridden" is its own typed error.
    assert_eq!(ed.apply(pe(PageEdit::RemoveOverride { page: "page-1".into(), item: "item-1".into() })).unwrap_err().code(), "item_not_overridden");
    assert_eq!(ed.apply(pe(PageEdit::DetachItem { page: "page-1".into(), item: "item-1".into() })).unwrap_err().code(), "item_not_overridden");
    // Detach: the copy stays a plain frame, the inherited item stays hidden on page 1.
    ed.apply(pe(PageEdit::OverrideItem { page: "page-1".into(), item: "item-1".into() })).unwrap();
    ed.apply(pe(PageEdit::DetachItem { page: "page-1".into(), item: "item-1".into() })).unwrap();
    assert_eq!(ed.page_items("page-1").unwrap(), vec![]);
    assert_eq!(ed.page_items("page-2").unwrap(), vec![("item-1".to_owned(), "parent:P1".to_owned())]);
    assert_eq!(ed.doc().document.layers.len(), 3);

    // Insert a page after page 1 (it takes page 1's parent), then a new spread after page 2.
    ed.apply(pe(PageEdit::InsertPage { after: "page-1".into(), new_spread: false, parent: Some("P1".into()) })).unwrap();
    ed.apply(pe(PageEdit::InsertPage { after: "page-2".into(), new_spread: true, parent: None })).unwrap();
    assert_eq!(page_order(&ed), vec![(0, "page-1".into()), (0, "page-3".into()), (0, "page-2".into()), (1, "page-4".into())]);
    assert_eq!(ed.doc().document.page_spreads[0].bounds.width.value, 600.0);
    assert_eq!(ed.page_rows()[1].side, PageSide::LeftHand);
    assert_eq!(ed.page_items("page-3").unwrap(), vec![("item-1".to_owned(), "parent:P1".to_owned())]);
    // Move page 4 into the first spread at index 0; its now-empty spread disappears.
    ed.apply(pe(PageEdit::MovePage { page: "page-4".into(), spread: 0, index: 0 })).unwrap();
    assert_eq!(ed.doc().document.page_spreads.len(), 1);
    assert_eq!(page_order(&ed)[0], (0, "page-4".into()));

    // Thread frame 1 (page 1) -> frame on page 3 -> frame on page 4; deleting page 3 closes the thread.
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-3".into(), x: 10.0, y: 10.0, w: 130.0, h: 32.0 }).unwrap();
    let f3 = ed.selected.clone().unwrap();
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-4".into(), x: 10.0, y: 10.0, w: 130.0, h: 32.0 }).unwrap();
    let f4 = ed.selected.clone().unwrap();
    ed.apply(LayoutEdit::Thread { from: id("SLYR", 1), to: f3.clone() }).unwrap();
    ed.apply(LayoutEdit::Thread { from: f3.clone(), to: f4.clone() }).unwrap();
    ed.apply(pe(PageEdit::DeletePage { page: "page-3".into() })).unwrap();
    let next_of_1 = ed.doc().document.layers.iter().find(|l| l.layer_id == id("SLYR", 1)).unwrap().text_frame.as_ref().unwrap().next_text_frame.clone();
    assert_eq!(next_of_1, Some(f4.clone()));
    assert!(ed.records().parents.binding("page-3").is_none());
    let steps = ed.history_len().0;
    let last = (ed.doc().clone(), ed.records().clone());

    // Refusals change nothing.
    let refused = [
        (pe(PageEdit::ApplyParent { pages: vec!["page-1".into()], parent: Some("P9".into()) }), "unknown_parent"),
        (pe(PageEdit::NewParent { name: " ".into(), based_on: None }), "invalid_name"),
        (pe(PageEdit::NewParent { name: "B".into(), based_on: Some("P7".into()) }), "unknown_parent"),
        (pe(PageEdit::OverrideItem { page: "page-1".into(), item: "item-9".into() }), "unknown_parent_item"),
        (pe(PageEdit::InsertPage { after: "page-9".into(), new_spread: false, parent: None }), "unknown_page"),
        (pe(PageEdit::MovePage { page: "page-1".into(), spread: 0, index: 9 }), "invalid_geometry"),
        (pe(PageEdit::AddParentItem { parent: "P1".into(), frame: id("SLYR", 99) }), "unknown_frame"),
    ];
    for (edit, code) in refused {
        assert_eq!(ed.apply(edit).unwrap_err().code(), code);
        assert_eq!((ed.doc(), ed.records(), ed.history_len().0), (&last.0, &last.1, steps));
    }
    // Every edit was one undo step: undoing all restores the original document and records.
    while ed.undo() {}
    assert_eq!((ed.doc(), ed.records()), (&original, &rec0));
    // Deleting down to one page: the last page is refused.
    let mut one = LayoutEditor::new(doc());
    one.apply(pe(PageEdit::DeletePage { page: "page-2".into() })).unwrap();
    assert_eq!(one.apply(pe(PageEdit::DeletePage { page: "page-1".into() })).unwrap_err().code(), "last_page");
}

#[test]
fn pages_panel_draws_its_ids_and_the_inherited_overlay() {
    let mut ed = LayoutEditor::new(doc());
    ed.apply(pe(PageEdit::NewParent { name: "A-Parent".into(), based_on: None })).unwrap();
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-2".into(), x: 20.0, y: 150.0, w: 100.0, h: 20.0 }).unwrap();
    let header = ed.selected.clone().unwrap();
    ed.apply(pe(PageEdit::AddParentItem { parent: "P1".into(), frame: header })).unwrap();
    ed.apply(pe(PageEdit::ApplyParent { pages: vec!["page-1".into(), "page-2".into()], parent: Some("P1".into()) })).unwrap();
    ed.selected_page = Some("page-1".into());
    let (drawn, shapes) = ui_frames(&mut ed);
    for w in [
        widget::PAGES_PANEL,
        widget::PAGE_INSERT,
        widget::PAGE_INSERT_SPREAD,
        widget::PAGE_DELETE,
        widget::PAGE_MOVE_UP,
        widget::PAGE_MOVE_DOWN,
        widget::PARENT_LIST,
        widget::PARENT_NEW,
        widget::PARENT_APPLY,
        widget::PARENT_ITEMS,
        widget::PAGE_VIEW,
    ] {
        assert!(drawn.contains(&w), "{w} drawn");
    }
    // The inherited header shows on both pages (orange overlay strokes), not as a page frame.
    assert_eq!(rects_of(&shapes, egui::Color32::from_rgb(200, 120, 0), false), 2, "{:?}", rect_colours(&shapes));
    let s = ed.state_json();
    assert_eq!(s["pages"][0]["parent"], "P1");
    assert_eq!(s["pages"][1]["items"], serde_json::json!([["item-1", "parent:P1"]]));
    assert_eq!(s["parents"][0]["name"], "A-Parent");
}
