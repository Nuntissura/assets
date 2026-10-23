//! Tables panel (lane SMB-LAYOUT-UI-2 slice 4; STU-LAY-035/141): insert a table at a paragraph
//! start of the selected story, insert/delete rows (header, body, footer) and columns, merge cells,
//! edit cell text, delete the table. The table is Folio's `StudioTable` in
//! `hsk.studio.longdoc_records@1` with its cell text in a cell story (one paragraph per cell line);
//! every result is checked by Folio's `validate_longdoc_records` (grid, spans, row order, sizes), and
//! a story carrying tables composes through layout's `reflow_with_tables` (text before the table,
//! the table rows, text resuming below it; header repetition, row breaks and overset rows are the
//! engine's). The page view draws the composed cells.
//!
//! Declared gaps: cell and table styles, strokes and fills from the panel, column resize by drag,
//! split cells, nested tables, graphic cells; a story with tables composes through
//! `reflow_with_tables`, which does not read the layout-flow records (columns/span/split are not
//! applied to it). InDesign table behaviour UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::{LayoutDoc, LayoutEdit, LayoutEditor, LayoutRecords, LayoutUiError, len, mint, preview_paragraphs, story_text, widget};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    LayoutEdges, StoryContent, StoryParagraph, StoryRange, StudioByteRange, StudioCellEdges, StudioEdgePriorities, StudioTable,
    StudioTableCell, StudioTableRow, StudioTextStory, TableRowType, TableStartRow, Unresolved, VerticalJustification,
    validate_longdoc_records,
};
use hsk_studio_layout::{
    AnchoredTable, Composer, Limits, ReflowRequest, ShapedParagraph, SourceSpan, SpaceParams, StoryWithTables, StyleSource,
    TableLimits, TypesetOptions, reflow_with_tables,
};
use serde_json::{Value, json};

/// Rows or columns one panel insert accepts.
pub const MAX_PANEL_TABLE_SIZE: u32 = 256;

/// Stroke colour of composed cells in the page view (also the headless visual assertions).
pub const TABLE_CELL_COLOUR: egui::Color32 = egui::Color32::from_rgb(0, 150, 150);

/// Keeps table anchors on their text when story bytes `[start, end)` become `new_len` bytes:
/// anchors after the edit shift, anchors inside it move to its start.
pub(crate) fn shift_anchors(r: &mut LayoutRecords, story: &str, start: u64, end: u64, new_len: u64) {
    for t in r.longdoc.tables.iter_mut().filter(|t| t.story_id == story) {
        if t.anchor >= end && !(start == end && t.anchor == start) {
            t.anchor = t.anchor.saturating_sub(end.saturating_sub(start)).saturating_add(new_len);
        } else if t.anchor > start && t.anchor < end {
            t.anchor = start;
        }
    }
}

/// One tables-panel command.
#[derive(Clone, Debug, PartialEq)]
pub enum TableEdit {
    /// A `rows` x `columns` table (the first `header_rows` are header rows) anchored at the start
    /// of `paragraph` of `story`, columns sharing the width of the story's first frame.
    InsertTable { story: String, paragraph: usize, rows: u32, columns: u32, header_rows: u32 },
    InsertRow { table: String, at: u32, row_type: TableRowType },
    DeleteRow { table: String, row: u32 },
    InsertColumn { table: String, at: u32 },
    DeleteColumn { table: String, column: u32 },
    /// The cell whose top-left slot is (`row`, `column`) gets this text (`\n` = new paragraph).
    SetCellText { table: String, row: u32, column: u32, text: String },
    /// Merges the `rows` x `columns` block at (`row`, `column`); texts join with newlines.
    MergeCells { table: String, row: u32, column: u32, rows: u32, columns: u32 },
    DeleteTable { table: String },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct TablesUi {
    pub rows: u32,
    pub columns: u32,
    pub cell: (u32, u32),
    pub text: String,
    pub selected: Option<String>,
}

/// Dense working copy of a table's cells with their texts.
struct Draft {
    cells: Vec<(StudioTableCell, String)>,
}

fn new_cell(row: u32, column: u32) -> StudioTableCell {
    StudioTableCell {
        row,
        column,
        row_span: 1,
        column_span: 1,
        content: None,
        insets: LayoutEdges { top: len(2.0), right: len(2.0), bottom: len(2.0), left: len(2.0) },
        fill: None,
        fill_tint: None,
        strokes: StudioCellEdges::default(),
        stroke_priorities: StudioEdgePriorities::default(),
        vertical_justification: VerticalJustification::Top,
    }
}

fn new_row(row_type: TableRowType) -> StudioTableRow {
    StudioTableRow { row_type, minimum_height: len(15.0), maximum_height: len(1000.0), keep_with_next_row: false, start_row: TableStartRow::Anywhere, allow_split: false }
}

fn draft(d: &LayoutDoc, t: &StudioTable) -> Draft {
    let text = t.cell_story_id.as_deref().and_then(|id| d.stories.iter().find(|s| s.story_id == id)).map(story_text).unwrap_or_default();
    Draft {
        cells: t
            .cells
            .iter()
            .map(|c| {
                let body = c
                    .content
                    .and_then(|r| text.get(usize::try_from(r.start).ok()?..usize::try_from(r.end).ok()?))
                    .unwrap_or_default();
                (c.clone(), body.strip_suffix('\n').unwrap_or(body).to_owned())
            })
            .collect(),
    }
}

/// Writes the draft back: cells in grid order, cell story rebuilt. Every text line of a cell is a
/// paragraph whose partition includes its newline, and a cell's content range covers its
/// paragraphs with their newlines, so the shaped cell paragraphs (`cell_paragraphs`) tile each
/// content range exactly as `layout_table` requires (`cell_content_not_on_paragraphs` otherwise).
fn commit(d: &mut LayoutDoc, t: &mut StudioTable, mut dr: Draft) -> Result<(), LayoutUiError> {
    dr.cells.sort_by_key(|(c, _)| (c.row, c.column));
    let mut text = String::new();
    let mut ranges = Vec::with_capacity(dr.cells.len());
    for (_, body) in &dr.cells {
        if body.is_empty() {
            ranges.push(None);
            continue;
        }
        let start = text.len() as u64;
        for line in body.split('\n') {
            text.push_str(line);
            text.push('\n');
        }
        ranges.push(Some((start, text.len() as u64)));
    }
    if text.ends_with('\n') {
        text.pop();
    }
    let total = text.len() as u64;
    t.cells = dr
        .cells
        .into_iter()
        .zip(ranges)
        .map(|((mut c, _), r)| {
            c.content = r.map(|(s, e)| StudioByteRange { start: s, end: e.min(total) });
            c
        })
        .collect();
    let mut parts = Vec::new();
    let mut at = 0u64;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            let end = (i as u64).saturating_add(1);
            parts.push(StoryParagraph { start: at, end, style_ref: None, paragraph_direction: None });
            at = end;
        }
    }
    if at < total || parts.is_empty() {
        parts.push(StoryParagraph { start: at, end: total, style_ref: None, paragraph_direction: None });
    }
    let id = match &t.cell_story_id {
        Some(id) => id.clone(),
        None => {
            let id = mint(d, "STXT")?;
            t.cell_story_id = Some(id.clone());
            id
        }
    };
    let story = StudioTextStory {
        content: StoryContent::Inline { text_utf8: text },
        paragraphs: parts,
        character_ranges: vec![StoryRange { start: 0, end: total, style_ref: None, applied_language: None }],
        ..super::empty_story(id.clone())
    };
    match d.stories.iter_mut().find(|s| s.story_id == id) {
        Some(slot) => *slot = story,
        None => d.stories.push(story),
    }
    Ok(())
}

fn table_mut<'a>(r: &'a mut LayoutRecords, id: &str) -> Result<&'a mut StudioTable, LayoutUiError> {
    r.longdoc.tables.iter_mut().find(|t| t.table_id == id).ok_or_else(|| LayoutUiError::UnknownTable(id.to_owned()))
}

fn validate(d: &LayoutDoc, r: &LayoutRecords) -> Result<(), LayoutUiError> {
    validate_longdoc_records(&r.longdoc, &d.document, hsk_studio_folio::Budget::default(), &CancellationToken::default(), &Unresolved)
        .map(|_| ())
        .map_err(|e| {
            // Grid failures carry check_table_grid's reason as the last target segment.
            let why = e.target.rsplit('/').next().unwrap_or_default();
            match why {
                "table_empty" => LayoutUiError::Table("table_empty"),
                "cells_overlap" => LayoutUiError::Table("cells_overlap"),
                "span_crosses_row_type" => LayoutUiError::Table("span_crosses_row_type"),
                "span_overflow" => LayoutUiError::Table("span_overflow"),
                "row_order" => LayoutUiError::Table("row_order"),
                "table_too_large" => LayoutUiError::Table("table_too_large"),
                _ => LayoutUiError::Folio("folio_invalid_table", e.target),
            }
        })
}

/// The state after one tables edit (pure; Folio-validated).
pub(crate) fn transform(doc: &LayoutDoc, rec: &LayoutRecords, edit: &TableEdit) -> Result<(LayoutDoc, LayoutRecords), LayoutUiError> {
    let mut d = doc.clone();
    let mut r = rec.clone();
    let size_ok = |n: u32| (1..=MAX_PANEL_TABLE_SIZE).contains(&n);
    match edit {
        TableEdit::InsertTable { story, paragraph, rows, columns, header_rows } => {
            if !size_ok(*rows) || !size_ok(*columns) || header_rows > rows {
                return Err(LayoutUiError::Table("table_too_large"));
            }
            let st = d.stories.iter().find(|s| s.story_id == *story).ok_or_else(|| LayoutUiError::UnknownStory(story.clone()))?;
            let anchor = st.paragraphs.get(*paragraph).map(|p| p.start).ok_or(LayoutUiError::Layout(hsk_studio_layout::LayoutError::InvalidInput))?;
            let frame = d
                .document
                .layers
                .iter()
                .find(|l| matches!(&l.payload, hsk_studio_folio::Payload::Text { story_id } if story_id == story) && l.text_frame.as_ref().is_some_and(|f| f.previous_text_frame.is_none()))
                .and_then(|l| l.text_frame.as_ref())
                .ok_or_else(|| LayoutUiError::UnknownFrame(story.clone()))?;
            let inner = frame.bounds.width.value - frame.inset_spacing.left.value - frame.inset_spacing.right.value;
            let w = inner / f64::from(*columns);
            if !w.is_finite() || w <= 0.0 {
                return Err(LayoutUiError::InvalidGeometry);
            }
            let mut table_id = None;
            for n in 1u64..=1_000_000 {
                let k = format!("T{n}");
                if !r.longdoc.tables.iter().any(|t| t.table_id == k) {
                    table_id = Some(k);
                    break;
                }
            }
            let mut t = StudioTable {
                table_id: table_id.ok_or(LayoutUiError::IdSpaceExhausted)?,
                story_id: story.clone(),
                anchor,
                cell_story_id: None,
                column_widths: (0..*columns).map(|_| len(w)).collect(),
                rows: (0..*rows).map(|i| new_row(if i < *header_rows { TableRowType::Header } else { TableRowType::Body })).collect(),
                cells: Vec::new(),
                break_headers: Default::default(),
                break_footers: Default::default(),
                skip_first_header: false,
                skip_last_footer: false,
                stroke_order: Default::default(),
            };
            let cells = (0..*rows).flat_map(|row| (0..*columns).map(move |c| (new_cell(row, c), String::new()))).collect();
            commit(&mut d, &mut t, Draft { cells })?;
            r.longdoc.tables.push(t);
        }
        TableEdit::DeleteTable { table } => {
            let t = table_mut(&mut r, table)?.clone();
            r.longdoc.tables.retain(|x| x.table_id != *table);
            if let Some(cs) = t.cell_story_id {
                d.stories.retain(|s| s.story_id != cs);
            }
        }
        TableEdit::InsertRow { table, at, row_type } => {
            let mut t = table_mut(&mut r, table)?.clone();
            if *at as usize > t.rows.len() || t.rows.len() >= MAX_PANEL_TABLE_SIZE as usize {
                return Err(LayoutUiError::Table("span_overflow"));
            }
            let mut dr = draft(&d, &t);
            let cols = t.column_widths.len() as u32;
            let mut covered = vec![false; cols as usize];
            for (c, _) in &mut dr.cells {
                if c.row >= *at {
                    c.row = c.row.saturating_add(1);
                } else if c.row.saturating_add(c.row_span) > *at {
                    // A span across the new row grows over it (same row type only).
                    c.row_span = c.row_span.saturating_add(1);
                    for k in c.column..c.column.saturating_add(c.column_span) {
                        if let Some(s) = covered.get_mut(k as usize) {
                            *s = true;
                        }
                    }
                }
            }
            for k in 0..cols {
                if !covered.get(k as usize).copied().unwrap_or(false) {
                    dr.cells.push((new_cell(*at, k), String::new()));
                }
            }
            t.rows.insert(*at as usize, new_row(*row_type));
            commit(&mut d, &mut t, dr)?;
            *table_mut(&mut r, table)? = t;
        }
        TableEdit::DeleteRow { table, row } => {
            let mut t = table_mut(&mut r, table)?.clone();
            if *row as usize >= t.rows.len() {
                return Err(LayoutUiError::Table("span_overflow"));
            }
            let mut dr = draft(&d, &t);
            dr.cells.retain(|(c, _)| !(c.row == *row && c.row_span == 1));
            for (c, _) in &mut dr.cells {
                if c.row > *row {
                    c.row = c.row.saturating_sub(1);
                } else if c.row.saturating_add(c.row_span) > *row {
                    c.row_span = c.row_span.saturating_sub(1);
                }
            }
            t.rows.remove(*row as usize);
            commit(&mut d, &mut t, dr)?;
            *table_mut(&mut r, table)? = t;
        }
        TableEdit::InsertColumn { table, at } => {
            let mut t = table_mut(&mut r, table)?.clone();
            if *at as usize > t.column_widths.len() || t.column_widths.len() >= MAX_PANEL_TABLE_SIZE as usize {
                return Err(LayoutUiError::Table("span_overflow"));
            }
            let mut dr = draft(&d, &t);
            let rows = t.rows.len() as u32;
            let mut covered = vec![false; rows as usize];
            for (c, _) in &mut dr.cells {
                if c.column >= *at {
                    c.column = c.column.saturating_add(1);
                } else if c.column.saturating_add(c.column_span) > *at {
                    c.column_span = c.column_span.saturating_add(1);
                    for k in c.row..c.row.saturating_add(c.row_span) {
                        if let Some(s) = covered.get_mut(k as usize) {
                            *s = true;
                        }
                    }
                }
            }
            for k in 0..rows {
                if !covered.get(k as usize).copied().unwrap_or(false) {
                    dr.cells.push((new_cell(k, *at), String::new()));
                }
            }
            let w = t.column_widths.get((*at as usize).saturating_sub(1)).or(t.column_widths.first()).cloned().unwrap_or_else(|| len(36.0));
            t.column_widths.insert(*at as usize, w);
            commit(&mut d, &mut t, dr)?;
            *table_mut(&mut r, table)? = t;
        }
        TableEdit::DeleteColumn { table, column } => {
            let mut t = table_mut(&mut r, table)?.clone();
            if *column as usize >= t.column_widths.len() {
                return Err(LayoutUiError::Table("span_overflow"));
            }
            let mut dr = draft(&d, &t);
            dr.cells.retain(|(c, _)| !(c.column == *column && c.column_span == 1));
            for (c, _) in &mut dr.cells {
                if c.column > *column {
                    c.column = c.column.saturating_sub(1);
                } else if c.column.saturating_add(c.column_span) > *column {
                    c.column_span = c.column_span.saturating_sub(1);
                }
            }
            t.column_widths.remove(*column as usize);
            commit(&mut d, &mut t, dr)?;
            *table_mut(&mut r, table)? = t;
        }
        TableEdit::SetCellText { table, row, column, text } => {
            let mut t = table_mut(&mut r, table)?.clone();
            let mut dr = draft(&d, &t);
            let slot = dr.cells.iter_mut().find(|(c, _)| c.row == *row && c.column == *column).ok_or(LayoutUiError::Table("cell_missing"))?;
            slot.1 = text.clone();
            commit(&mut d, &mut t, dr)?;
            *table_mut(&mut r, table)? = t;
        }
        TableEdit::MergeCells { table, row, column, rows, columns } => {
            if *rows == 0 || *columns == 0 || (*rows == 1 && *columns == 1) {
                return Err(LayoutUiError::Table("span_zero"));
            }
            let mut t = table_mut(&mut r, table)?.clone();
            let (r1, c1) = (row.saturating_add(*rows), column.saturating_add(*columns));
            let mut dr = draft(&d, &t);
            // Every cell must lie wholly inside or wholly outside the block.
            let mut texts = Vec::new();
            for (c, body) in &dr.cells {
                let (cr1, cc1) = (c.row.saturating_add(c.row_span), c.column.saturating_add(c.column_span));
                let overlaps = c.row < r1 && *row < cr1 && c.column < c1 && *column < cc1;
                let inside = c.row >= *row && cr1 <= r1 && c.column >= *column && cc1 <= c1;
                if overlaps && !inside {
                    return Err(LayoutUiError::Table("cells_overlap"));
                }
                if inside && !body.is_empty() {
                    texts.push(body.clone());
                }
            }
            dr.cells.retain(|(c, _)| !(c.row >= *row && c.row < r1 && c.column >= *column && c.column < c1) || (c.row == *row && c.column == *column));
            let top_left = dr.cells.iter_mut().find(|(c, _)| c.row == *row && c.column == *column).ok_or(LayoutUiError::Table("cell_missing"))?;
            top_left.0.row_span = *rows;
            top_left.0.column_span = *columns;
            top_left.1 = texts.join("\n");
            commit(&mut d, &mut t, dr)?;
            *table_mut(&mut r, table)? = t;
        }
    }
    validate(&d, &r)?;
    Ok((d, r))
}

/// Preview shaping of a cell story (12 pt, as `preview_paragraphs`) over whole paragraph
/// partitions, newline included, so cell content ranges are tiled exactly.
fn cell_paragraphs(st: &StudioTextStory) -> Result<Vec<ShapedParagraph>, LayoutUiError> {
    let text = story_text(st);
    st.paragraphs
        .iter()
        .map(|p| {
            ShapedParagraph::from_fixed_advance(text, SourceSpan { start: p.start, end: p.end }, 6.0, 15.0, 9.0)
                .ok_or(LayoutUiError::Layout(hsk_studio_layout::LayoutError::InvalidInput))
        })
        .collect()
}

impl LayoutEditor {
    /// Tables anchored in a story in flow order (anchor, then record order): the order of
    /// `reflow_with_tables`' proposals.
    pub fn tables_of(&self, story: &str) -> Vec<&StudioTable> {
        let mut v: Vec<&StudioTable> = self.rec.longdoc.tables.iter().filter(|t| t.story_id == story).collect();
        v.sort_by_key(|t| t.anchor);
        v
    }

    /// A story with its anchored tables through layout's `reflow_with_tables` (preview shaping).
    pub fn compose_tables(&self, story_id: &str) -> Result<StoryWithTables, LayoutUiError> {
        let st = self.doc.stories.iter().find(|s| s.story_id == story_id).ok_or_else(|| LayoutUiError::UnknownStory(story_id.to_owned()))?;
        let paras = preview_paragraphs(st, &self.doc.styles)?;
        let tables = self.tables_of(story_id);
        let mut cell_inputs = Vec::with_capacity(tables.len());
        for t in &tables {
            let cs = t.cell_story_id.as_deref().and_then(|id| self.doc.stories.iter().find(|s| s.story_id == id));
            let cp = match cs {
                Some(s) => cell_paragraphs(s)?,
                None => Vec::new(),
            };
            cell_inputs.push((*t, cs, cp));
        }
        let anchored: Vec<AnchoredTable<'_>> = cell_inputs
            .iter()
            .map(|(t, cs, cp)| AnchoredTable { table: t, cell_story: *cs, cell_resolved_text: None, cell_paragraphs: cp })
            .collect();
        let req = ReflowRequest {
            document: &self.doc.document,
            story: st,
            resolved_text: None,
            paragraphs: &paras,
            obstacles: &[],
            composer: Composer::FirstFit,
            space: SpaceParams::default(),
            expected_revision: self.doc.document.revision,
            limits: Limits::default(),
        };
        let opts = TypesetOptions { styles: Some(StyleSource { records: &self.doc.styles, default_font_size_pt: 12.0 }), ..TypesetOptions::default() };
        reflow_with_tables(&req, &anchored, TableLimits::default(), &opts, &CancellationToken::default()).map_err(|e| LayoutUiError::Table(e.code()))
    }

    pub(crate) fn tables_json(&self) -> Value {
        Value::Array(
            self.rec
                .longdoc
                .tables
                .iter()
                .map(|t| {
                    let composed = self.compose_tables(&t.story_id);
                    // Proposals follow `tables_of` (flow order).
                    let k = self.tables_of(&t.story_id).iter().position(|x| x.table_id == t.table_id);
                    let placed = composed.as_ref().ok().and_then(|c| c.tables.get(k?)).map(|p| (p.cells.len(), p.overset_row));
                    json!({
                        "table_id": t.table_id, "story_id": t.story_id, "anchor": t.anchor,
                        "rows": t.rows.len(), "columns": t.column_widths.len(), "cells": t.cells.len(),
                        "placed_cells": placed.map(|p| p.0), "overset_row": placed.and_then(|p| p.1),
                        "refusal": composed.as_ref().err().map(|e| e.code()),
                    })
                })
                .collect(),
        )
    }

    /// Cell rectangles of the composed tables in spread coordinates: (spread, x, y, w, h).
    #[allow(clippy::arithmetic_side_effects)]
    pub(crate) fn table_cell_rects(&self) -> Vec<(usize, f64, f64, f64, f64)> {
        let mut out = Vec::new();
        let stories: std::collections::BTreeSet<&str> = self.rec.longdoc.tables.iter().map(|t| t.story_id.as_str()).collect();
        for s in stories {
            let Ok(c) = self.compose_tables(s) else { continue };
            for p in &c.tables {
                for cell in &p.cells {
                    let Some(part) = p.parts.get(cell.part) else { continue };
                    let r = cell.rect;
                    out.push((part.spread, part.origin_x_pt + r.x0_pt, part.origin_y_pt + r.y0_pt, r.x1_pt - r.x0_pt, r.y1_pt - r.y0_pt));
                }
            }
        }
        out
    }

    /// The tables panel.
    pub(crate) fn tables_panel(&mut self, ui: &mut egui::Ui, drawn: &mut Vec<&'static str>) {
        let story = self.selected.as_deref().and_then(|f| self.story_of_frame(f)).map(|s| s.story_id.clone());
        ui.push_id(widget::TABLES_PANEL, |ui| {
            ui.label("Tables");
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut self.tables_ui.rows).range(1..=MAX_PANEL_TABLE_SIZE).prefix("rows "));
                ui.add(egui::DragValue::new(&mut self.tables_ui.columns).range(1..=MAX_PANEL_TABLE_SIZE).prefix("columns "));
                if ui.push_id(widget::TABLE_INSERT, |ui| ui.add_enabled(story.is_some(), egui::Button::new("Insert table"))).inner.clicked()
                    && let Some(s) = story.clone()
                {
                    let (rows, columns) = (self.tables_ui.rows.max(1), self.tables_ui.columns.max(1));
                    self.act(LayoutEdit::Table(TableEdit::InsertTable { story: s, paragraph: 0, rows, columns, header_rows: 0 }));
                }
            });
            let tables: Vec<(String, usize, usize)> = story.as_deref().map(|s| self.tables_of(s).iter().map(|t| (t.table_id.clone(), t.rows.len(), t.column_widths.len())).collect()).unwrap_or_default();
            for (id, rows, cols) in tables {
                ui.horizontal(|ui| {
                    if ui.selectable_label(self.tables_ui.selected.as_deref() == Some(id.as_str()), format!("{id} {rows}x{cols}")).clicked() {
                        self.tables_ui.selected = Some(id.clone());
                    }
                    let last_row = u32::try_from(rows).unwrap_or(u32::MAX);
                    let last_col = u32::try_from(cols).unwrap_or(u32::MAX);
                    if ui.button("+row").clicked() {
                        self.act(LayoutEdit::Table(TableEdit::InsertRow { table: id.clone(), at: last_row, row_type: TableRowType::Body }));
                    }
                    if ui.button("+column").clicked() {
                        self.act(LayoutEdit::Table(TableEdit::InsertColumn { table: id.clone(), at: last_col }));
                    }
                    if ui.button("delete").clicked() {
                        self.act(LayoutEdit::Table(TableEdit::DeleteTable { table: id.clone() }));
                    }
                });
            }
            if let Some(t) = self.tables_ui.selected.clone() {
                ui.push_id(widget::TABLE_CELL, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut self.tables_ui.cell.0).prefix("row "));
                        ui.add(egui::DragValue::new(&mut self.tables_ui.cell.1).prefix("column "));
                        ui.text_edit_singleline(&mut self.tables_ui.text);
                        if ui.button("Set cell text").clicked() {
                            let (row, column) = self.tables_ui.cell;
                            self.act(LayoutEdit::Table(TableEdit::SetCellText { table: t.clone(), row, column, text: self.tables_ui.text.clone() }));
                        }
                    });
                });
                drawn.push(widget::TABLE_CELL);
            }
        });
        drawn.extend([widget::TABLES_PANEL, widget::TABLE_INSERT]);
    }
}
