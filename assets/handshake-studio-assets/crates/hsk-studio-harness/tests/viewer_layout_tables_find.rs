//! Lane SMB-LAYOUT-UI-2 slice 4: tables panel (STU-LAY-035/141) and find/change panel
//! (STU-LAY-027/129, text and GREP modes), headless (fixture in `layout_ui_fixture`).
//! InDesign table and find/change behaviour UNVERIFIED (D5).
#![cfg(feature = "layout-ui")]
mod layout_ui_fixture;
use hsk_studio_folio::{StoryContent, TableRowType};
use hsk_studio_harness::layout_ui::{FindQuery, FindScope, LayoutEdit, LayoutEditor, TableEdit, tables::TABLE_CELL_COLOUR, widget};
use hsk_studio_layout::FindMode;
use layout_ui_fixture::{TEXT, doc, id, rect_colours, rects_of, ui_frames};


fn te(e: TableEdit) -> LayoutEdit {
    LayoutEdit::Table(e)
}
fn text_of(ed: &LayoutEditor, story: &str) -> String {
    match &ed.doc().stories.iter().find(|s| s.story_id == story).unwrap().content {
        StoryContent::Inline { text_utf8 } => text_utf8.clone(),
        StoryContent::Stored { .. } => unreachable!(),
    }
}
fn q(pattern: &str, mode: FindMode, scope: FindScope) -> FindQuery {
    FindQuery { pattern: pattern.into(), mode, scope }
}
const TXT: FindMode = FindMode::Text { case_sensitive: false, whole_word: false };

#[test]
fn tables_insert_edit_merge_and_compose_through_layout() {
    let original = doc();
    let mut ed = LayoutEditor::new(original.clone());
    // A 2 x 2 table (one header row) in the empty story of a tall new frame on page 2.
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-2".into(), x: 10.0, y: 10.0, w: 130.0, h: 180.0 }).unwrap();
    let host = ed.story_of_frame(&ed.selected.clone().unwrap()).unwrap().story_id.clone();
    ed.apply(te(TableEdit::InsertTable { story: host.clone(), paragraph: 0, rows: 2, columns: 2, header_rows: 1 })).unwrap();
    let t = ed.tables_of(&host)[0].clone();
    assert_eq!((t.table_id.as_str(), t.rows.len(), t.column_widths.len(), t.cells.len(), t.column_widths[0].value), ("T1", 2, 2, 4, 65.0));
    assert_eq!(t.rows[0].row_type, TableRowType::Header);
    ed.apply(te(TableEdit::SetCellText { table: "T1".into(), row: 1, column: 1, text: "x\ny".into() })).unwrap();
    ed.apply(te(TableEdit::SetCellText { table: "T1".into(), row: 0, column: 0, text: "h".into() })).unwrap();
    let cs = ed.tables_of(&host)[0].cell_story_id.clone().unwrap();
    assert_eq!(text_of(&ed, &cs), "h\nx\ny");
    // Rows and columns: insert a body row at the end and a column at 1; delete the column again.
    ed.apply(te(TableEdit::InsertRow { table: "T1".into(), at: 2, row_type: TableRowType::Body })).unwrap();
    ed.apply(te(TableEdit::InsertColumn { table: "T1".into(), at: 1 })).unwrap();
    assert_eq!(ed.tables_of(&host)[0].cells.len(), 9);
    ed.apply(te(TableEdit::DeleteColumn { table: "T1".into(), column: 1 })).unwrap();
    // Merge the two body cells of column 0 (rows 1..3); texts join.
    ed.apply(te(TableEdit::SetCellText { table: "T1".into(), row: 2, column: 0, text: "z".into() })).unwrap();
    ed.apply(te(TableEdit::MergeCells { table: "T1".into(), row: 1, column: 0, rows: 2, columns: 1 })).unwrap();
    let t = ed.tables_of(&host)[0].clone();
    assert_eq!((t.cells.len(), t.cells.iter().find(|c| c.row == 1 && c.column == 0).unwrap().row_span), (5, 2));
    assert_eq!(text_of(&ed, &cs), "h\nz\nx\ny");
    // The story composes through reflow_with_tables: all five cells placed, no overset row.
    let c = ed.compose_tables(&host).unwrap();
    assert_eq!((c.tables[0].cells.len(), c.tables[0].overset_row), (5, None));
    let s = ed.state_json();
    assert_eq!((s["tables"][0]["rows"].as_u64(), s["tables"][0]["refusal"].is_null()), (Some(3), true));

    let steps = ed.history_len().0;
    let last = (ed.doc().clone(), ed.records().clone());
    let refused = [
        (te(TableEdit::MergeCells { table: "T1".into(), row: 0, column: 1, rows: 2, columns: 1 }), "span_crosses_row_type"),
        (te(TableEdit::MergeCells { table: "T1".into(), row: 2, column: 0, rows: 1, columns: 2 }), "cells_overlap"),
        (te(TableEdit::DeleteRow { table: "T1".into(), row: 9 }), "span_overflow"),
        (te(TableEdit::SetCellText { table: "T1".into(), row: 2, column: 0, text: "q".into() }), "cell_missing"),
        (te(TableEdit::InsertTable { story: host.clone(), paragraph: 0, rows: 0, columns: 2, header_rows: 0 }), "table_too_large"),
        (te(TableEdit::DeleteTable { table: "T9".into() }), "unknown_table"),
    ];
    for (edit, code) in refused {
        assert_eq!(ed.apply(edit).unwrap_err().code(), code);
        assert_eq!((ed.doc(), ed.records(), ed.history_len().0), (&last.0, &last.1, steps));
    }
    // Deleting the table removes its cell story; every edit was one undo step.
    ed.apply(te(TableEdit::DeleteTable { table: "T1".into() })).unwrap();
    assert!(ed.records().longdoc.tables.is_empty());
    assert!(ed.doc().stories.iter().all(|s| s.story_id != cs));
    while ed.undo() {}
    assert_eq!(ed.doc(), &original);
}

#[test]
fn find_change_text_and_grep_modes_are_single_undo_steps() {
    let original = doc();
    let mut ed = LayoutEditor::new(original.clone());
    let story = FindScope::Story(id("STXT", 1));
    // Text mode: case-insensitive literal; metacharacters are literal.
    assert_eq!(ed.find(&q("BBBB", TXT, story.clone())).unwrap().len(), 1);
    assert_eq!(ed.find(&q("a.a", TXT, story.clone())).unwrap().len(), 0);
    assert_eq!(ed.find(&q("BBBB", FindMode::Text { case_sensitive: true, whole_word: false }, story.clone())).unwrap().len(), 0);
    assert_eq!(ed.find(&q("bb", FindMode::Text { case_sensitive: false, whole_word: true }, story.clone())).unwrap().len(), 0);
    // GREP mode with capture groups: swap the halves of every four-letter word starting with c-e.
    let grep = FindMode::Grep { case_sensitive: true };
    let hits = ed.find(&q(r"([c-e])\1", grep, story.clone()));
    assert_eq!(hits.unwrap_err().code(), "grep_pattern_unsupported", "back-references are refused, never mis-matched");
    let hits = ed.find(&q(r"\b([c-e]{2})([c-e]{2})\b", grep, story.clone())).unwrap();
    assert_eq!(hits.iter().map(|h| h.start).collect::<Vec<_>>(), vec![10, 15, 20]);
    ed.apply(LayoutEdit::Change { query: q(r"\b([c-e]{2})([c-e]{2})\b", grep, story.clone()), replacement: "<$2-$1>".into(), only: None }).unwrap();
    assert_eq!(text_of(&ed, &id("STXT", 1)), TEXT.replace("cccc", "<cc-cc>").replace("dddd", "<dd-dd>").replace("eeee", "<ee-ee>"));
    assert_eq!(ed.history_len().0, 1, "change all is one undo step");
    // Change one hit only (the second "jjjj"-less word: gggg at its current offset).
    let g = ed.find(&q("gggg", TXT, story.clone())).unwrap()[0].clone();
    ed.apply(LayoutEdit::Change { query: q("gggg", TXT, story.clone()), replacement: "$1".into(), only: Some((g.story.clone(), g.start)) }).unwrap();
    assert!(text_of(&ed, &id("STXT", 1)).contains(" $1 "), "text mode replacements are literal");
    // Selection scope, typed refusals.
    assert_eq!(ed.find(&q("a", TXT, FindScope::Selection { story: id("STXT", 1), start: 0, end: 2 })).unwrap().len(), 2);
    assert_eq!(ed.find(&q("a", TXT, FindScope::Selection { story: id("STXT", 1), start: 3, end: 3 })).unwrap_err().code(), "no_text_for_find");
    assert_eq!(ed.find(&q("", TXT, story.clone())).unwrap_err().code(), "grep_pattern_invalid");
    assert_eq!(ed.find(&q("a", TXT, FindScope::Story("nope".into()))).unwrap_err().code(), "unknown_story");
    let mut empty = LayoutEditor::new(doc());
    empty.apply(LayoutEdit::DrawFrame { page_id: "page-2".into(), x: 0.0, y: 0.0, w: 50.0, h: 50.0 }).unwrap();
    let s2 = empty.story_of_frame(&empty.selected.clone().unwrap()).unwrap().story_id.clone();
    assert_eq!(empty.find(&q("a", TXT, FindScope::Story(s2))).unwrap_err().code(), "no_text_for_find");
    while ed.undo() {}
    assert_eq!(ed.doc(), &original);
}

#[test]
fn tables_and_find_panels_draw_and_cells_reach_the_page_view() {
    let mut ed = LayoutEditor::new(doc());
    ed.apply(te(TableEdit::InsertTable { story: id("STXT", 1), paragraph: 0, rows: 1, columns: 2, header_rows: 0 })).unwrap();
    ed.selected = Some(id("SLYR", 1));
    let (drawn, shapes) = ui_frames(&mut ed);
    for w in [widget::TABLES_PANEL, widget::TABLE_INSERT, widget::FIND_PANEL, widget::FIND_TEXT, widget::CHANGE_TEXT, widget::FIND_MODE, widget::FIND_NEXT, widget::CHANGE, widget::CHANGE_ALL, widget::FIND_HITS] {
        assert!(drawn.contains(&w), "{w} drawn");
    }
    assert_eq!(rects_of(&shapes, TABLE_CELL_COLOUR, false), 2, "two composed cells drawn: {:?}", rect_colours(&shapes));
}
