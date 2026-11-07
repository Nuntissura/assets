//! `layout.*`: every [`LayoutEdit`] member and the editor's undo, redo and preflight by command id.
//! Each edit is one named undo step of the layout editor; the first command opens the editor, as the
//! window's panel does. Layout edits live in the editor until the layout records persist.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::shell::Panel;
use super::decode::{Fields, folio_field, invalid, missing};
use super::CommandRefusal;
use crate::layout_ui::{FindQuery, FindScope, LayoutEdit, LayoutEditor, LayoutUiError, PageEdit, StyleEdit, StylePatch, TableEdit};
use hsk_studio_folio as folio;
use hsk_studio_layout::FindMode;
use hsk_studio_layout::Severity;
use serde_json::{Value, json};

fn refused(e: &LayoutUiError) -> CommandRefusal {
    CommandRefusal::new(e.code(), json!({"reason": format!("{e:?}")}))
}

/// A key that is present and null clears; a key that is absent leaves the field alone.
fn patched<T>(f: Fields<'_>, key: &str, read: impl FnOnce() -> Result<T, CommandRefusal>) -> Result<Option<Option<T>>, CommandRefusal> {
    match f.0.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(_) => read().map(|v| Some(Some(v))),
    }
}

fn style_patch(f: Fields<'_>) -> Result<StylePatch, CommandRefusal> {
    Ok(StylePatch {
        name: f.opt_text("name")?,
        based_on: patched(f, "based_on", || f.text("based_on"))?,
        next_style: patched(f, "next_style", || f.text("next_style"))?,
        font_family: patched(f, "font_family", || f.text("font_family"))?,
        font_size_pt: patched(f, "font_size_pt", || f.number("font_size_pt"))?,
        leading_pt: patched(f, "leading_pt", || f.number("leading_pt"))?,
        align: patched(f, "align", || folio_field!(f, "align", folio::StudioTextAlign, "a text alignment token"))?,
    })
}

fn find_query(f: Fields<'_>) -> Result<FindQuery, CommandRefusal> {
    let case_sensitive = f.flag_or("case_sensitive", false)?;
    let mode = match f.opt_text("mode")?.as_deref() {
        None | Some("text") => FindMode::Text { case_sensitive, whole_word: f.flag_or("whole_word", false)? },
        Some("grep") => FindMode::Grep { case_sensitive },
        Some(_) => return Err(invalid("mode", "text|grep")),
    };
    let scope = match f.opt_text("scope")?.as_deref() {
        None | Some("document") => FindScope::Document,
        Some("story") => FindScope::Story(f.text("story")?),
        Some("selection") => FindScope::Selection { story: f.text("story")?, start: f.unsigned("start")?, end: f.unsigned("end")? },
        Some(_) => return Err(invalid("scope", "document|story|selection")),
    };
    Ok(FindQuery { pattern: f.text("pattern")?, mode, scope })
}

/// The typed edit of one row; `None` when the row is not an edit.
fn edit_of(id: &str, f: Fields<'_>) -> Result<Option<LayoutEdit>, CommandRefusal> {
    let opt = |key: &str| f.opt_text(key);
    let text = |key: &str| f.text(key);
    Ok(Some(match id {
        "layout.draw_frame" => LayoutEdit::DrawFrame { page_id: text("page_id")?, x: f.number("x")?, y: f.number("y")?, w: f.number("w")?, h: f.number("h")? },
        "layout.thread" => LayoutEdit::Thread { from: text("from")?, to: text("to")? },
        "layout.unthread" => LayoutEdit::Unthread { from: text("from")? },
        "layout.set_columns" => LayoutEdit::SetColumns { frame: text("frame")?, count: f.count("count")?, gutter: f.number("gutter")? },
        "layout.replace_text" => LayoutEdit::ReplaceText { story: text("story")?, start: f.unsigned("start")?, end: f.unsigned("end")?, text: text("text")? },
        "layout.apply_paragraph_style" => LayoutEdit::ApplyParagraphStyle { story: text("story")?, paragraph: f.index("paragraph")?, style: opt("style")? },
        "layout.apply_character_style" => LayoutEdit::ApplyCharacterStyle { story: text("story")?, start: f.unsigned("start")?, end: f.unsigned("end")?, style: opt("style")? },
        "layout.page.insert_page" => LayoutEdit::Page(PageEdit::InsertPage { after: text("after")?, new_spread: f.flag_or("new_spread", false)?, parent: opt("parent")? }),
        "layout.page.delete_page" => LayoutEdit::Page(PageEdit::DeletePage { page: text("page")? }),
        "layout.page.move_page" => LayoutEdit::Page(PageEdit::MovePage { page: text("page")?, spread: f.index("spread")?, index: f.index("index")? }),
        "layout.page.new_parent" => LayoutEdit::Page(PageEdit::NewParent { name: text("name")?, based_on: opt("based_on")? }),
        "layout.page.apply_parent" => LayoutEdit::Page(PageEdit::ApplyParent { pages: f.list("pages")?, parent: opt("parent")? }),
        "layout.page.add_parent_item" => LayoutEdit::Page(PageEdit::AddParentItem { parent: text("parent")?, frame: text("frame")? }),
        "layout.page.override_item" => LayoutEdit::Page(PageEdit::OverrideItem { page: text("page")?, item: text("item")? }),
        "layout.page.remove_override" => LayoutEdit::Page(PageEdit::RemoveOverride { page: text("page")?, item: text("item")? }),
        "layout.page.detach_item" => LayoutEdit::Page(PageEdit::DetachItem { page: text("page")?, item: text("item")? }),
        "layout.style.new_style" => {
            let scope = f.token("scope", &[("paragraph", folio::TypeStyleScope::Paragraph), ("character", folio::TypeStyleScope::Character)])?;
            LayoutEdit::Style(StyleEdit::NewStyle { scope, name: text("name")?, based_on: opt("based_on")? })
        }
        "layout.style.edit_style" => {
            let patch = f.nested("patch", &["name", "based_on", "next_style", "font_family", "font_size_pt", "leading_pt", "align"], style_patch)?;
            LayoutEdit::Style(StyleEdit::EditStyle { style: text("style")?, patch })
        }
        "layout.style.delete_style" => LayoutEdit::Style(StyleEdit::DeleteStyle { style: text("style")?, replace_with: opt("replace_with")? }),
        "layout.style.add_grep_rule" => LayoutEdit::Style(StyleEdit::AddGrepRule { style: text("style")?, pattern: text("pattern")?, character_style: text("character_style")? }),
        "layout.style.remove_grep_rule" => LayoutEdit::Style(StyleEdit::RemoveGrepRule { style: text("style")?, index: f.index("index")? }),
        "layout.style.clear_overrides" => LayoutEdit::Style(StyleEdit::ClearOverrides { story: text("story")?, paragraph: f.index("paragraph")? }),
        "layout.table.insert_table" => LayoutEdit::Table(TableEdit::InsertTable { story: text("story")?, paragraph: f.index("paragraph")?, rows: f.count("rows")?, columns: f.count("columns")?, header_rows: f.count("header_rows")? }),
        "layout.table.insert_row" => {
            let row_type = f.token("row_type", &[("header", folio::TableRowType::Header), ("body", folio::TableRowType::Body), ("footer", folio::TableRowType::Footer)])?;
            LayoutEdit::Table(TableEdit::InsertRow { table: text("table")?, at: f.count("at")?, row_type })
        }
        "layout.table.delete_row" => LayoutEdit::Table(TableEdit::DeleteRow { table: text("table")?, row: f.count("row")? }),
        "layout.table.insert_column" => LayoutEdit::Table(TableEdit::InsertColumn { table: text("table")?, at: f.count("at")? }),
        "layout.table.delete_column" => LayoutEdit::Table(TableEdit::DeleteColumn { table: text("table")?, column: f.count("column")? }),
        "layout.table.set_cell_text" => LayoutEdit::Table(TableEdit::SetCellText { table: text("table")?, row: f.count("row")?, column: f.count("column")?, text: text("text")? }),
        "layout.table.merge_cells" => LayoutEdit::Table(TableEdit::MergeCells { table: text("table")?, row: f.count("row")?, column: f.count("column")?, rows: f.count("rows")?, columns: f.count("columns")? }),
        "layout.table.delete_table" => LayoutEdit::Table(TableEdit::DeleteTable { table: text("table")? }),
        "layout.change" => {
            let only = match (f.opt_text("only_story")?, f.has("only_start")) {
                (None, false) => None,
                (Some(story), true) => Some((story, f.unsigned("only_start")?)),
                (None, true) => return Err(missing("only_story")),
                (Some(_), false) => return Err(missing("only_start")),
            };
            LayoutEdit::Change { query: find_query(f)?, replacement: text("replacement")?, only }
        }
        _ => return Ok(None),
    }))
}

fn history(editor: &LayoutEditor) -> Value {
    let (undo, redo) = editor.history_len();
    json!({"undo_name": editor.undo_name(), "redo_name": editor.redo_name(), "undo_steps": undo, "redo_steps": redo})
}

pub(super) fn run(viewer: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Value, CommandRefusal> {
    // Decoded first: a refused edit opens no editor.
    let edit = edit_of(id, f)?;
    if viewer.layout_editor().is_none() {
        viewer.toggle_panel(Panel::LayoutEditor).map_err(|message| CommandRefusal::new("layout_editor_unavailable", json!({"message": message})))?;
    }
    let Some(editor) = viewer.layout_editor() else {
        return Err(CommandRefusal::new("layout_editor_unavailable", Value::Null));
    };
    if let Some(edit) = edit {
        editor.apply(edit).map_err(|e| refused(&e))?;
        return Ok(history(editor));
    }
    match id {
        "layout.undo" => {
            if !editor.undo() {
                return Err(CommandRefusal::new("nothing_to_undo", Value::Null));
            }
            Ok(history(editor))
        }
        "layout.redo" => {
            if !editor.redo() {
                return Err(CommandRefusal::new("nothing_to_redo", Value::Null));
            }
            Ok(history(editor))
        }
        "layout.find" => {
            let hits = editor.find(&find_query(f)?).map_err(|e| refused(&e))?;
            Ok(json!({"hits": hits.iter().map(|h| json!({"story": h.story, "start": h.start, "end": h.end, "replacement": h.replacement})).collect::<Vec<_>>()}))
        }
        "layout.set_preflight_severity" => {
            let severity = f.token("severity", &[("error", Severity::Error), ("warning", Severity::Warning), ("informational", Severity::Informational)])?;
            editor.set_preflight_severity(&f.text("rule")?, severity).map_err(|e| refused(&e))?;
            Ok(Value::Null)
        }
        "layout.preflight" => Ok(editor.preflight_json()),
        "layout.thread_info" => {
            let t = editor.thread_info(&f.text("frame")?).map_err(|e| refused(&e))?;
            Ok(json!({"frame_id": t.frame_id, "parent_story": t.parent_story, "start_text_frame": t.start_text_frame, "end_text_frame": t.end_text_frame, "previous_text_frame": t.previous_text_frame, "next_text_frame": t.next_text_frame, "text_frame_index": t.text_frame_index}))
        }
        other => Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    }
}
