//! Layout rows of the command corpus: every `LayoutEdit` member and the layout editor's undo, redo, find
//! and preflight, addressed by id through `Viewer::run_command`.

use super::{CommandEntry, ParamSpec, flag as b, index as i, list as l, number as n, one_of as o, output_path as out, path as file, record as r, text as t, typed};

const EDIT: &str = "LayoutEditor::apply";
const PAGE_ITEM: &[ParamSpec] = &[t("page"), t("item")];
const FIND_SCOPE: &str = "document|story|selection";
const FIND: &[ParamSpec] = &[t("pattern"), o("mode", "text|grep").opt(), b("case_sensitive").opt(), b("whole_word").opt(), o("scope", FIND_SCOPE).opt(), t("story").opt(), i("start").opt(), i("end").opt()];
const CHANGE: &[ParamSpec] = &[t("pattern"), t("replacement"), o("mode", "text|grep").opt(), b("case_sensitive").opt(), b("whole_word").opt(), o("scope", FIND_SCOPE).opt(), t("story").opt(), i("start").opt(), i("end").opt(), t("only_story").opt(), i("only_start").opt()];

pub(super) const ROWS: &[CommandEntry] = &[
    typed("layout.draw_frame", EDIT, "Draw text frame", true, &[t("page_id"), n("x"), n("y"), n("w"), n("h")]),
    typed("layout.thread", EDIT, "Thread text frames", true, &[t("from"), t("to")]),
    typed("layout.unthread", EDIT, "Unthread text frame", true, &[t("from")]),
    typed("layout.set_columns", EDIT, "Set text frame columns", true, &[t("frame"), i("count"), n("gutter")]),
    typed("layout.replace_text", EDIT, "Replace story text", true, &[t("story"), i("start"), i("end"), t("text")]),
    typed("layout.apply_paragraph_style", EDIT, "Apply paragraph style", true, &[t("story"), i("paragraph"), t("style").opt()]),
    typed("layout.apply_character_style", EDIT, "Apply character style", true, &[t("story"), i("start"), i("end"), t("style").opt()]),
    typed("layout.page.insert_page", EDIT, "Insert page", true, &[t("after"), b("new_spread").opt(), t("parent").opt()]),
    typed("layout.page.delete_page", EDIT, "Delete page", true, &[t("page")]),
    typed("layout.page.move_page", EDIT, "Move page", true, &[t("page"), i("spread"), i("index")]),
    typed("layout.page.new_parent", EDIT, "New parent page", true, &[t("name"), t("based_on").opt()]),
    typed("layout.page.apply_parent", EDIT, "Apply parent page", true, &[l("pages"), t("parent").opt()]),
    typed("layout.page.add_parent_item", EDIT, "Add parent page item", true, &[t("parent"), t("frame")]),
    typed("layout.page.override_item", EDIT, "Override parent item", true, PAGE_ITEM),
    typed("layout.page.remove_override", EDIT, "Remove parent item override", true, PAGE_ITEM),
    typed("layout.page.detach_item", EDIT, "Detach parent item", true, PAGE_ITEM),
    typed("layout.style.new_style", EDIT, "New type style", true, &[o("scope", "paragraph|character"), t("name"), t("based_on").opt()]),
    typed("layout.style.edit_style", EDIT, "Edit type style", true, &[t("style"), r("patch", "{name, based_on, next_style, font_family, font_size_pt, leading_pt, align}; null clears, absent keeps")]),
    typed("layout.style.delete_style", EDIT, "Delete type style", true, &[t("style"), t("replace_with").opt()]),
    typed("layout.style.add_grep_rule", EDIT, "Add GREP style rule", true, &[t("style"), t("pattern"), t("character_style")]),
    typed("layout.style.remove_grep_rule", EDIT, "Remove GREP style rule", true, &[t("style"), i("index")]),
    typed("layout.style.clear_overrides", EDIT, "Clear paragraph overrides", true, &[t("story"), i("paragraph")]),
    typed("layout.table.insert_table", EDIT, "Insert table", true, &[t("story"), i("paragraph"), i("rows"), i("columns"), i("header_rows")]),
    typed("layout.table.insert_row", EDIT, "Insert table row", true, &[t("table"), i("at"), o("row_type", "header|body|footer")]),
    typed("layout.table.delete_row", EDIT, "Delete table row", true, &[t("table"), i("row")]),
    typed("layout.table.insert_column", EDIT, "Insert table column", true, &[t("table"), i("at")]),
    typed("layout.table.delete_column", EDIT, "Delete table column", true, &[t("table"), i("column")]),
    typed("layout.table.set_cell_text", EDIT, "Set table cell text", true, &[t("table"), i("row"), i("column"), t("text")]),
    typed("layout.table.merge_cells", EDIT, "Merge table cells", true, &[t("table"), i("row"), i("column"), i("rows"), i("columns")]),
    typed("layout.table.delete_table", EDIT, "Delete table", true, &[t("table")]),
    typed("layout.change", EDIT, "Find and change", true, CHANGE),
    typed("layout.undo", "LayoutEditor::undo", "Undo layout edit", true, &[]),
    typed("layout.redo", "LayoutEditor::redo", "Redo layout edit", true, &[]),
    typed("layout.find", "LayoutEditor::find", "Find in stories", false, FIND),
    typed("layout.preflight", "LayoutEditor::preflight", "Run preflight", false, &[]),
    typed("layout.set_preflight_severity", "LayoutEditor::set_preflight_severity", "Set preflight rule severity", false, &[t("rule"), o("severity", "error|warning|informational")]),
    typed("layout.thread_info", "LayoutEditor::thread_info", "Text thread information", false, &[t("frame")]),
    // Lane SMB-LAYOUT-PDF-HOST-1: the layout's output (harness-view view/layout_export.rs). None changes
    // the document: a new blank layout is editor state, exports are derived deliverables.
    typed("layout.open", "Viewer::layout_open", "Open the layout editor", false, &[n("page_width_pt").opt(), n("page_height_pt").opt(), i("pages").opt()]),
    typed("layout.export_pdf", "Viewer::export_layout_pdf", "Export layout as PDF", false, &[
        out("path"), b("overwrite").opt(), o("pdfx", "pdfx.none|pdfx.x4_2010").opt(), t("created").opt(), file("output_profile").opt(),
        t("output_condition_identifier").opt(), t("output_condition").opt(), t("registry").opt(), t("title").opt(), b("subset_fonts").opt(), b("ignore_preflight").opt(),
    ]),
    typed("layout.export_idml", "Viewer::export_layout_idml", "Export layout as IDML", false, &[out("path"), b("overwrite").opt()]),
    typed("layout.render_pdf_page", "Viewer::render_pdf_page", "Render a PDF page to PNG", false, &[file("source"), i("page"), n("px_per_pt"), out("path"), b("overwrite").opt()]),
];
