//! List and table formatting commands for the rich-text editor.

use crate::commands::{
    apply, block_at, caret_block_path, caret_in_block_at_offset, clamp_selection, enclosing_kind,
    path_is_inside_kind, reparent_caret_into, split_block_path, CommandContext, CommandError,
};
use handshake_rich_document_model::node::{BlockNode, Child, NodeKind};
use handshake_rich_document_model::position::DocPosition;
use handshake_rich_document_model::selection::Selection;
use handshake_rich_document_model::transform::Step;
use serde_json::Value as JsonValue;

// ── lists ───────────────────────────────────────────────────────────────────────

/// Toggle the caret's paragraph into a list of `list_kind` (bullet/ordered) holding a
/// single `list_item`, or back to a paragraph when already inside one. Matches React
/// `toggleBulletList` / `toggleOrderedList`. Only converts a top-level paragraph
/// (guarded — a caret already inside a list toggles back out).
pub(crate) fn toggle_list(ctx: &mut CommandContext<'_>, list_kind: NodeKind) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let (parent_path, index) = split_block_path(&block_path)?;

    // Already a list_item inside a list of this kind at the parent chain? -> toggle off.
    if let Some(list_path) = enclosing_list(ctx.doc, &block_path) {
        let list = block_at(ctx.doc, &list_path).ok_or(CommandError::NoCaret)?;
        if list.kind == list_kind {
            return unwrap_list_to_paragraph(ctx, &list_path);
        }
    }

    let block = block_at(ctx.doc, &block_path).ok_or(CommandError::NoCaret)?;
    if !matches!(block.kind, NodeKind::Paragraph | NodeKind::Heading(_)) {
        return Err(CommandError::InvalidContext {
            reason: format!(
                "toggle_list only converts a paragraph/heading, not {:?}",
                block.kind
            ),
        });
    }
    let para = block.clone();
    let list_item = BlockNode::with_children(NodeKind::ListItem, vec![Child::Block(para)]);
    let list = BlockNode::with_children(list_kind, vec![Child::Block(list_item)]);
    apply(
        ctx,
        vec![
            Step::DeleteNode {
                parent_path: parent_path.clone(),
                index,
            },
            Step::InsertNode {
                parent_path: parent_path.clone(),
                index,
                node: list,
            },
        ],
    )?;
    // Caret descends two levels: block -> list[index] / item[0] / para[0].
    let mut new_block_path = parent_path.clone();
    new_block_path.extend_from_slice(&[index, 0, 0]);
    reparent_caret_into(ctx, &block_path, &new_block_path);
    Ok(())
}

/// Toggle a task list: like [`toggle_list`] but the items are `task_item`s carrying
/// `attrs.checked: false` (the Tiptap `taskItem` shape — backend anchor). Toggling off
/// converts the task list back to a paragraph.
pub(crate) fn toggle_task_list(ctx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let (parent_path, index) = split_block_path(&block_path)?;

    if let Some(list_path) = enclosing_list(ctx.doc, &block_path) {
        // A bullet/ordered list holding task_items is our task-list representation;
        // toggling off unwraps regardless of which list kind hosts the task items.
        let list = block_at(ctx.doc, &list_path).ok_or(CommandError::NoCaret)?;
        let holds_task = list
            .children
            .iter()
            .filter_map(Child::as_block)
            .any(|c| matches!(c.kind, NodeKind::TaskItem));
        if holds_task {
            return unwrap_list_to_paragraph(ctx, &list_path);
        }
    }

    let block = block_at(ctx.doc, &block_path).ok_or(CommandError::NoCaret)?;
    if !matches!(block.kind, NodeKind::Paragraph | NodeKind::Heading(_)) {
        return Err(CommandError::InvalidContext {
            reason: format!(
                "toggle_task_list only converts a paragraph/heading, not {:?}",
                block.kind
            ),
        });
    }
    let para = block.clone();
    let mut task_item = BlockNode::with_children(NodeKind::TaskItem, vec![Child::Block(para)]);
    task_item
        .attrs
        .insert("checked".to_string(), JsonValue::Bool(false));
    // Task items live inside a bullet list (Tiptap TaskList renders as a list whose
    // items are taskItems; the MT-011 schema allows TaskItem as a list child).
    let list = BlockNode::with_children(NodeKind::BulletList, vec![Child::Block(task_item)]);
    apply(
        ctx,
        vec![
            Step::DeleteNode {
                parent_path: parent_path.clone(),
                index,
            },
            Step::InsertNode {
                parent_path: parent_path.clone(),
                index,
                node: list,
            },
        ],
    )?;
    let mut new_block_path = parent_path.clone();
    new_block_path.extend_from_slice(&[index, 0, 0]);
    reparent_caret_into(ctx, &block_path, &new_block_path);
    Ok(())
}

/// Toggle the `checked` attr of the task item enclosing the caret (false<->true).
/// Implemented as a DeleteNode + InsertNode of the task_item with the flipped attr
/// (the MT-011 transform has no in-place attr step). Errors if the caret is not inside
/// a task item.
pub(crate) fn toggle_task_item_checked(ctx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let task_path = enclosing_kind(ctx.doc, &block_path, NodeKind::TaskItem).ok_or(
        CommandError::InvalidContext {
            reason: "toggle_task_item_checked requires the caret inside a task item".to_string(),
        },
    )?;
    let (parent_path, index) = split_block_path(&task_path)?;
    let task = block_at(ctx.doc, &task_path).ok_or(CommandError::NoCaret)?;
    let mut new_task = task.clone();
    let now = !new_task.task_checked();
    new_task
        .attrs
        .insert("checked".to_string(), JsonValue::Bool(now));
    apply(
        ctx,
        vec![
            Step::DeleteNode {
                parent_path: parent_path.clone(),
                index,
            },
            Step::InsertNode {
                parent_path,
                index,
                node: new_task,
            },
        ],
    )?;
    clamp_selection(ctx);
    Ok(())
}

/// Sink (indent) the list item enclosing the caret: nest it as a child of the previous
/// sibling list item (the ProseMirror `sinkListItem` shape). Requires a previous
/// sibling item (you cannot indent the first item). The increased nesting depth is the
/// "indent level" the AC checks.
pub(crate) fn sink_list_item(ctx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let item_path =
        enclosing_list_item(ctx.doc, &block_path).ok_or(CommandError::InvalidContext {
            reason: "sink_list_item requires the caret inside a list item".to_string(),
        })?;
    let (list_path, item_index) = split_block_path(&item_path)?;
    if item_index == 0 {
        return Err(CommandError::InvalidContext {
            reason: "cannot sink the first list item (no previous sibling to nest under)"
                .to_string(),
        });
    }
    let list = block_at(ctx.doc, &list_path).ok_or(CommandError::NoCaret)?;
    let list_kind = list.kind;
    let item = list.children[item_index]
        .as_block()
        .ok_or(CommandError::NoCaret)?
        .clone();
    // The previous sibling item gets a nested sub-list (of the same kind) appended,
    // holding the sunk item. Rebuild the previous item with the nested list.
    let prev = list.children[item_index - 1]
        .as_block()
        .ok_or(CommandError::NoCaret)?
        .clone();
    let mut new_prev = prev.clone();
    // If the previous item already ends in a sub-list of this kind, append to it;
    // otherwise add a new sub-list. (Keeps repeated indents from stacking empty lists.)
    if let Some(Child::Block(last)) = new_prev.children.last_mut() {
        if last.kind == list_kind {
            last.children.push(Child::Block(item.clone()));
        } else {
            let sub = BlockNode::with_children(list_kind, vec![Child::Block(item.clone())]);
            new_prev.children.push(Child::Block(sub));
        }
    } else {
        let sub = BlockNode::with_children(list_kind, vec![Child::Block(item.clone())]);
        new_prev.children.push(Child::Block(sub));
    }

    apply(
        ctx,
        vec![
            // Remove the sunk item from the list.
            Step::DeleteNode {
                parent_path: list_path.clone(),
                index: item_index,
            },
            // Replace the previous item with the rebuilt one carrying the nested list.
            Step::DeleteNode {
                parent_path: list_path.clone(),
                index: item_index - 1,
            },
            Step::InsertNode {
                parent_path: list_path.clone(),
                index: item_index - 1,
                node: new_prev,
            },
        ],
    )?;
    // Move the caret INTO the sunk item's new home: prev_item -> last child (the nested
    // sub-list) -> last item -> its first block -> first text leaf. Walk the live tree to
    // resolve the concrete indices (the sub-list index inside the prev item is its last
    // child; the sunk item is the sub-list's last item).
    let prev_item_path = {
        let mut p = list_path.clone();
        p.push(item_index - 1);
        p
    };
    if let Some(caret) = caret_into_nested_sunk_item(ctx.doc, &prev_item_path) {
        *ctx.selection = Selection::caret(caret);
    } else {
        clamp_selection(ctx);
    }
    Ok(())
}

/// After a sink, resolve a caret inside the sunk item's new home: the previous item's
/// LAST child must be the nested sub-list, whose LAST item holds the sunk content. Land
/// the caret at the start of that item's first inline-content block.
fn caret_into_nested_sunk_item(doc: &BlockNode, prev_item_path: &[usize]) -> Option<DocPosition> {
    let prev_item = block_at(doc, prev_item_path)?;
    let sub_list_idx = prev_item.children.len().checked_sub(1)?;
    let sub_list = prev_item.children.get(sub_list_idx)?.as_block()?;
    if !matches!(sub_list.kind, NodeKind::BulletList | NodeKind::OrderedList) {
        return None;
    }
    let sunk_item_idx = sub_list.children.len().checked_sub(1)?;
    let sunk_item = sub_list.children.get(sunk_item_idx)?.as_block()?;
    // The sunk item's first block child is the paragraph; caret to its first text leaf.
    let _ = sunk_item.children.first()?.as_block()?;
    let mut block_path = prev_item_path.to_vec();
    block_path.extend_from_slice(&[sub_list_idx, sunk_item_idx, 0]);
    caret_in_block_at_offset(doc, &block_path, 0)
}

/// Lift (dedent) the list item enclosing the caret: if it is nested inside a sub-list
/// (a child of a parent list item), pull it up to be a sibling of its grandparent item.
/// Requires the item to be nested at least one level (else it is already at the top
/// list level and there is nothing to lift to within the list).
pub(crate) fn lift_list_item(ctx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let item_path =
        enclosing_list_item(ctx.doc, &block_path).ok_or(CommandError::InvalidContext {
            reason: "lift_list_item requires the caret inside a list item".to_string(),
        })?;
    // item_path = [.., grandparent_list_item, sub_list_index, item_index]
    // To lift, the item's list must itself be inside a parent list ITEM.
    let (sub_list_path, item_index) = split_block_path(&item_path)?;
    let (parent_item_path, _sub_list_index_in_item) = split_block_path(&sub_list_path)?;
    let parent_item = block_at(ctx.doc, &parent_item_path);
    let is_nested = parent_item
        .map(|b| matches!(b.kind, NodeKind::ListItem | NodeKind::TaskItem))
        .unwrap_or(false);
    if !is_nested {
        return Err(CommandError::InvalidContext {
            reason: "cannot lift a top-level list item (already at the outer list level)"
                .to_string(),
        });
    }
    let (outer_list_path, parent_item_index) = split_block_path(&parent_item_path)?;
    let item = block_at(ctx.doc, &item_path)
        .ok_or(CommandError::NoCaret)?
        .clone();
    apply(
        ctx,
        vec![
            // Remove the item from its sub-list.
            Step::DeleteNode {
                parent_path: sub_list_path.clone(),
                index: item_index,
            },
            // Insert it as the next sibling of the parent item in the outer list.
            Step::InsertNode {
                parent_path: outer_list_path.clone(),
                index: parent_item_index + 1,
                node: item,
            },
        ],
    )?;
    clamp_selection(ctx);
    Ok(())
}

// ── tables (MC-003 nesting guard) ────────────────────────────────────────────────

/// The default `attrs` for a freshly-inserted table cell, matching the Tiptap
/// table-cell shape (`colspan: 1`, `rowspan: 1`). `is_header` stamps the
/// header-row marker attr (the MT-011 model has no `tableHeader` NodeKind, so header
/// status is an attr — see module docs).
fn table_cell(is_header: bool) -> BlockNode {
    let mut cell = BlockNode::with_children(
        NodeKind::TableCell,
        vec![Child::Block(BlockNode::paragraph(""))],
    );
    cell.attrs.insert("colspan".to_string(), JsonValue::from(1));
    cell.attrs.insert("rowspan".to_string(), JsonValue::from(1));
    if is_header {
        cell.attrs
            .insert("isHeader".to_string(), JsonValue::Bool(true));
    }
    cell
}

/// Insert a `rows`x`cols` table AFTER the caret's block, with a header first row
/// (Tiptap `insertTable({withHeaderRow:true})` parity). Guard (MC-003): refuses when
/// the caret is already inside a table (no nested tables in the backend schema).
/// `rows`/`cols` are clamped to `>= 1`.
pub(crate) fn insert_table(ctx: &mut CommandContext<'_>, rows: u8, cols: u8) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    if path_is_inside_kind(ctx.doc, &block_path, NodeKind::Table) {
        return Err(CommandError::InvalidContext {
            reason: "cannot insert a table inside an existing table (no nested tables)".to_string(),
        });
    }
    let (parent_path, index) = split_block_path(&block_path)?;
    let rows = rows.max(1);
    let cols = cols.max(1);
    let mut table_rows = Vec::with_capacity(rows as usize);
    for r in 0..rows {
        let is_header = r == 0;
        let mut row = BlockNode::new(NodeKind::TableRow);
        for _ in 0..cols {
            row.children.push(Child::Block(table_cell(is_header)));
        }
        table_rows.push(Child::Block(row));
    }
    let table = BlockNode::with_children(NodeKind::Table, table_rows);
    apply(
        ctx,
        vec![Step::InsertNode {
            parent_path,
            index: index + 1,
            node: table,
        }],
    )?;
    // Move the caret into the first cell's paragraph text leaf.
    let mut new_block_path = caret_block_path(ctx.selection).unwrap_or_default();
    let (pp, _) = split_block_path(&new_block_path).unwrap_or((Vec::new(), 0));
    // table is at pp[index+1]; first row[0] / cell[0] / para[0] / text[0].
    new_block_path = pp;
    new_block_path.extend_from_slice(&[index + 1, 0, 0, 0, 0]);
    *ctx.selection = Selection::caret(DocPosition::new(new_block_path, 0));
    clamp_selection(ctx);
    Ok(())
}

/// Add a row before/after the caret's row inside a table. Errors if the caret is not
/// inside a table. The new row mirrors the column count of the caret's row and uses
/// non-header cells.
pub(crate) fn add_row(ctx: &mut CommandContext<'_>, after: bool) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let row_path =
        enclosing_kind(ctx.doc, &block_path, NodeKind::TableRow).ok_or(not_in_table("add_row"))?;
    let (table_path, row_index) = split_block_path(&row_path)?;
    let row = block_at(ctx.doc, &row_path).ok_or(CommandError::NoCaret)?;
    let cols = row.children.len().max(1);
    let mut new_row = BlockNode::new(NodeKind::TableRow);
    for _ in 0..cols {
        new_row.children.push(Child::Block(table_cell(false)));
    }
    let index = if after { row_index + 1 } else { row_index };
    apply(
        ctx,
        vec![Step::InsertNode {
            parent_path: table_path,
            index,
            node: new_row,
        }],
    )?;
    clamp_selection(ctx);
    Ok(())
}

/// Delete the caret's row from its table. Errors if the caret is not in a table. If the
/// table would become empty (last row), the whole table is deleted instead (so a table
/// is never left with zero rows — an invalid empty table).
pub(crate) fn delete_row(ctx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let row_path = enclosing_kind(ctx.doc, &block_path, NodeKind::TableRow)
        .ok_or(not_in_table("delete_row"))?;
    let (table_path, row_index) = split_block_path(&row_path)?;
    let table = block_at(ctx.doc, &table_path).ok_or(CommandError::NoCaret)?;
    if table.children.len() <= 1 {
        // Last row -> delete the whole table (no empty tables).
        return delete_table(ctx);
    }
    apply(
        ctx,
        vec![Step::DeleteNode {
            parent_path: table_path,
            index: row_index,
        }],
    )?;
    // Caret may now point past the end; clamp it to the table start.
    move_caret_to_table_start(ctx, &block_path);
    Ok(())
}

/// Add a column before/after the caret's column across every row of the table. Errors
/// if the caret is not in a table. New cells are non-header.
pub(crate) fn add_col(ctx: &mut CommandContext<'_>, after: bool) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let cell_path =
        enclosing_kind(ctx.doc, &block_path, NodeKind::TableCell).ok_or(not_in_table("add_col"))?;
    // cell_path = [.., table, row, cell]; the cell index is the column index.
    let (row_path, col_index) = split_block_path(&cell_path)?;
    let (table_path, _row_index) = split_block_path(&row_path)?;
    let table = block_at(ctx.doc, &table_path).ok_or(CommandError::NoCaret)?;
    let insert_at = if after { col_index + 1 } else { col_index };
    let n_rows = table.children.len();
    // Build one InsertNode step per row (each adds the column cell at the same index).
    let mut steps = Vec::with_capacity(n_rows);
    for r in 0..n_rows {
        let mut rp = table_path.clone();
        rp.push(r);
        let is_header = r == 0; // first row is the header row
        steps.push(Step::InsertNode {
            parent_path: rp,
            index: insert_at,
            node: table_cell(is_header),
        });
    }
    apply(ctx, steps)?;
    clamp_selection(ctx);
    Ok(())
}

/// Delete the caret's column across every row. Errors if not in a table. If a row would
/// become empty (last column), the whole table is deleted (no zero-column rows).
pub(crate) fn delete_col(ctx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let cell_path = enclosing_kind(ctx.doc, &block_path, NodeKind::TableCell)
        .ok_or(not_in_table("delete_col"))?;
    let (row_path, col_index) = split_block_path(&cell_path)?;
    let (table_path, _row_index) = split_block_path(&row_path)?;
    let table = block_at(ctx.doc, &table_path).ok_or(CommandError::NoCaret)?;
    let n_rows = table.children.len();
    // If every row has a single column, deleting it empties the table.
    let min_cols = table
        .children
        .iter()
        .filter_map(Child::as_block)
        .map(|r| r.children.len())
        .min()
        .unwrap_or(0);
    if min_cols <= 1 {
        return delete_table(ctx);
    }
    // Delete the cell at col_index in every row (same index across rows).
    let mut steps = Vec::with_capacity(n_rows);
    for r in 0..n_rows {
        let mut rp = table_path.clone();
        rp.push(r);
        steps.push(Step::DeleteNode {
            parent_path: rp,
            index: col_index,
        });
    }
    apply(ctx, steps)?;
    move_caret_to_table_start(ctx, &block_path);
    Ok(())
}

/// Delete the entire table the caret is in. Errors if not in a table.
pub(crate) fn delete_table(ctx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let table_path = enclosing_kind(ctx.doc, &block_path, NodeKind::Table)
        .ok_or(not_in_table("delete_table"))?;
    let (parent_path, index) = split_block_path(&table_path)?;
    apply(
        ctx,
        vec![Step::DeleteNode {
            parent_path: parent_path.clone(),
            index,
        }],
    )?;
    // Park the caret at the doc start (the table is gone). Resolve to the first leaf.
    if let Some(pos) = handshake_rich_document_model::position::resolve(ctx.doc, 0) {
        *ctx.selection = Selection::caret(pos);
    }
    Ok(())
}

/// Toggle the header marker on the caret's table's FIRST row: flips every cell's
/// `attrs.isHeader` between true and absent (the MT-011-model representation of the
/// React `toggleHeaderRow`). Errors if not in a table.
pub(crate) fn toggle_header_row(ctx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let table_path = enclosing_kind(ctx.doc, &block_path, NodeKind::Table)
        .ok_or(not_in_table("toggle_header_row"))?;
    let table = block_at(ctx.doc, &table_path).ok_or(CommandError::NoCaret)?;
    let Some(first_row) = table.children.first().and_then(Child::as_block) else {
        return Err(not_in_table("toggle_header_row"));
    };
    // Determine the current header state from the first cell.
    let currently_header = first_row
        .children
        .first()
        .and_then(Child::as_block)
        .map(|c| {
            c.attrs
                .get("isHeader")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false)
        })
        .unwrap_or(false);

    // Rebuild the first row with toggled header cells (DeleteNode + InsertNode of the row).
    let mut new_row = first_row.clone();
    for child in new_row.children.iter_mut() {
        if let Child::Block(cell) = child {
            if currently_header {
                cell.attrs.remove("isHeader");
            } else {
                cell.attrs
                    .insert("isHeader".to_string(), JsonValue::Bool(true));
            }
        }
    }
    let mut row_path = table_path.clone();
    row_path.push(0);
    let (rp_parent, rp_index) = split_block_path(&row_path)?;
    apply(
        ctx,
        vec![
            Step::DeleteNode {
                parent_path: rp_parent.clone(),
                index: rp_index,
            },
            Step::InsertNode {
                parent_path: rp_parent,
                index: rp_index,
                node: new_row,
            },
        ],
    )?;
    clamp_selection(ctx);
    Ok(())
}

/// The path of the nearest enclosing `list_item` OR `task_item` (the indent/dedent
/// commands treat both as list items).
fn enclosing_list_item(doc: &BlockNode, block_path: &[usize]) -> Option<Vec<usize>> {
    for end in (0..=block_path.len()).rev() {
        let prefix = &block_path[..end];
        if let Some(b) = block_at(doc, prefix) {
            if matches!(b.kind, NodeKind::ListItem | NodeKind::TaskItem) {
                return Some(prefix.to_vec());
            }
        }
    }
    None
}

/// The path of the nearest enclosing list (`bullet_list` or `ordered_list`).
fn enclosing_list(doc: &BlockNode, block_path: &[usize]) -> Option<Vec<usize>> {
    for end in (0..=block_path.len()).rev() {
        let prefix = &block_path[..end];
        if let Some(b) = block_at(doc, prefix) {
            if matches!(b.kind, NodeKind::BulletList | NodeKind::OrderedList) {
                return Some(prefix.to_vec());
            }
        }
    }
    None
}

/// Unwrap a single-item list back to its inner paragraph at the list's position. Used
/// by [`toggle_list`] / [`toggle_task_list`] to toggle off. Replaces the list node with
/// the first item's first block child (the paragraph).
fn unwrap_list_to_paragraph(
    ctx: &mut CommandContext<'_>,
    list_path: &[usize],
) -> Result<(), CommandError> {
    let (parent_path, index) = split_block_path(list_path)?;
    let list = block_at(ctx.doc, list_path).ok_or(CommandError::NoCaret)?;
    let first_item = list
        .children
        .first()
        .and_then(Child::as_block)
        .ok_or(CommandError::NoCaret)?;
    let inner = first_item
        .children
        .first()
        .and_then(Child::as_block)
        .ok_or(CommandError::NoCaret)?
        .clone();
    apply(
        ctx,
        vec![
            Step::DeleteNode {
                parent_path: parent_path.clone(),
                index,
            },
            Step::InsertNode {
                parent_path: parent_path.clone(),
                index,
                node: inner,
            },
        ],
    )?;
    // Caret moves up two levels (list/item/para -> para): land it in the paragraph leaf.
    let mut new_block_path = parent_path.clone();
    new_block_path.push(index);
    let caret = caret_in_block_at_offset(ctx.doc, &new_block_path, 0).unwrap_or_else(|| {
        DocPosition::new(
            {
                let mut p = new_block_path.clone();
                p.push(0);
                p
            },
            0,
        )
    });
    *ctx.selection = Selection::caret(caret);
    clamp_selection(ctx);
    Ok(())
}

/// Move the caret to the start of the table enclosing `block_path` (first cell's
/// paragraph), used after a row/column delete that may have invalidated the caret.
fn move_caret_to_table_start(ctx: &mut CommandContext<'_>, block_path: &[usize]) {
    if let Some(table_path) = enclosing_kind(ctx.doc, block_path, NodeKind::Table) {
        // table/row0/cell0/para0/text0
        let mut p = table_path;
        p.extend_from_slice(&[0, 0, 0, 0]);
        if let Some(pos) = position_for_first_leaf(ctx.doc, &p) {
            *ctx.selection = Selection::caret(pos);
            return;
        }
    }
    clamp_selection(ctx);
}

/// Build a caret DocPosition addressing the first text leaf at `leaf_path` if it
/// resolves, else `None`.
fn position_for_first_leaf(doc: &BlockNode, leaf_path: &[usize]) -> Option<DocPosition> {
    let (leaf_idx, block_path) = leaf_path.split_last()?;
    let block = block_at(doc, block_path)?;
    block.children.get(*leaf_idx)?.as_text()?;
    Some(DocPosition::new(leaf_path.to_vec(), 0))
}

/// Build the "not inside a table" context error for a table-edit command.
fn not_in_table(cmd: &str) -> CommandError {
    CommandError::InvalidContext {
        reason: format!("{cmd} requires the caret inside a table"),
    }
}
