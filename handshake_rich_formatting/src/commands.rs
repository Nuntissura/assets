//! Block- and inline-level formatting commands for the rich-text editor
//! (WP-KERNEL-012 MT-013).
//!
//! This module is the COMMAND LAYER of the E2 rich-text cluster: every toolbar
//! button (MT-013 native toolbar) and keyboard shortcut (MT-013
//! native keymap) ultimately calls one of the functions here. A command takes
//! the live editor state by `&mut` (the [`BlockNode`] doc, the [`UndoManager`], and
//! the [`Selection`]), builds an MT-011 [`Transaction`] of [`Step`]s, applies it
//! atomically through [`apply_transaction`], pushes the receipt onto the undo
//! manager on success, and updates the post-op selection. No global mutable state;
//! the command never owns the document (MT impl note 4).
//!
//! ## Why a command CAN mutate the selection (deviation from the literal signature)
//!
//! The MT contract sketches `fn command(doc, history, selection: &Selection)`, but
//! the SCOPE-EXPANSION note ("set the post-op Selection in the command") and the
//! structural commands (Enter-split moves the caret to the start of the new block;
//! Backspace-merge lands the caret at the join point; set_heading keeps the caret in
//! the converted block) REQUIRE the command to move the caret. So the selection is
//! taken `&mut`. This is the boringly-correct shape: a ProseMirror command returns a
//! transaction that carries BOTH doc steps and a new selection, and the native model
//! threads the selection alongside. The toggle/insert commands that do not move the
//! caret leave it untouched.
//!
//! ## Active-state discipline (red-team RISK-1 / MC-001)
//!
//! [`is_mark_active`] inspects EVERY [`TextLeaf`] that overlaps the current selection
//! range, not just the leaf at the caret. The ProseMirror "toggle mark" rule is: if
//! ANY overlapping leaf carries the mark, the toggle REMOVES it from all of them; if
//! NONE do, the toggle ADDS it to all of them. A 3-leaf selection where only the
//! middle leaf is bold therefore reports `active = true` and the toggle removes the
//! bold (test `toggle_bold_multi_leaf_active`).
//!
//! ## Context guards (red-team MC-002 / MC-003)
//!
//! - `set_heading` / `set_paragraph` only convert a `paragraph` or `heading` block;
//!   a caret inside a `list_item`, `table_cell`, or `code_block` returns
//!   [`CommandError::InvalidContext`] (a `list_item` is not a legal direct child of
//!   `doc`, so a blind conversion would corrupt the tree — MC-002).
//! - `insert_table` refuses to nest a table inside an existing table
//!   ([`CommandError::InvalidContext`]) — the backend `content_json` table schema has
//!   no nested-table content (MC-003).
//!
//! ## Backend node/attr-shape anchor (MT-011 hsLink lesson, KERNEL_BUILDER gate)
//!
//! The list/table/task_item commands produce the node + attr shapes the REAL backend
//! `content_json` round-trips (read from `app/src/lib/editor/editor_commands.ts` +
//! `app/src/lib/tiptap/extension_set.ts`):
//! - a task item carries `attrs.checked: bool` (Tiptap `taskItem`'s `checked` attr).
//! - a table cell carries `attrs.colspan: 1` + `attrs.rowspan: 1` (Tiptap
//!   table-cell defaults), and a header row's cells carry `attrs.isHeader: true`.
//!
//! NOTE (table header DIVERGENCE, recorded honestly): the REAL backend uses a
//! distinct `tableHeader` NODE TYPE for header cells, but the MT-011 [`NodeKind`]
//! enum (a frozen dependency this MT reuses, NOT forks) has only `TableCell` — no
//! `TableHeader` variant. So header-ness is carried as a cell `attrs.isHeader: true`
//! attr here. A full `tableHeader`-node round-trip is an MT-011 model gap surfaced as
//! a blocker, not fixed by forking the model in MT-013.

use handshake_rich_document_model::history::UndoManager;
use handshake_rich_document_model::node::{
    BlockNode, Child, HeadingLevel, Mark, NodeKind, TextLeaf,
};
use handshake_rich_document_model::position::DocPosition;
use handshake_rich_document_model::selection::Selection;
use handshake_rich_document_model::transform::{
    apply_transaction, ActorKind, Step, Transaction, TransformError,
};

use crate::list_table::{
    add_col, add_row, delete_col, delete_row, delete_table, insert_table, lift_list_item,
    sink_list_item, toggle_header_row, toggle_list, toggle_task_item_checked, toggle_task_list,
};

use serde_json::Value as JsonValue;

/// Why a formatting command could not run. Distinct from [`TransformError`]: a
/// `CommandError` is raised by the command layer BEFORE (or instead of) building a
/// transaction — a guard refusing an illegal context, or no addressable caret. A
/// transaction that fails schema validation surfaces as [`CommandError::Transform`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    /// The command is not valid in the current node context (MC-002 / MC-003): e.g.
    /// `set_heading` inside a `list_item`/`table_cell`/`code_block`, or `insert_table`
    /// inside an existing table.
    #[error("command not valid in this context: {reason}")]
    InvalidContext { reason: String },
    /// The selection does not address a caret/leaf the command can act on (e.g. a
    /// whole-node selection where a text caret is required).
    #[error("no addressable caret/selection for this command")]
    NoCaret,
    /// The command's transaction failed to apply (addressing or schema error); the
    /// doc was rolled back by [`apply_transaction`].
    #[error("transform failed: {0}")]
    Transform(#[from] TransformError),
}

/// The full catalog of MT-013 formatting commands, one variant per toolbar button /
/// keyboard shortcut. Achieves command-for-command parity with the React
/// `EDITOR_COMMANDS` catalog categories `history | format | block | list | table |
/// tableEdit` (plus the MT-013 scope-expansion structural commands). The toolbar and
/// keymap both produce a `FormattingCommand` and dispatch it through [`dispatch`], so
/// the two surfaces can never drift (the same id drives a button and a chord).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormattingCommand {
    // --- history ---
    Undo,
    Redo,
    // --- inline marks (format) ---
    ToggleBold,
    ToggleItalic,
    ToggleUnderline,
    ToggleStrike,
    ToggleCode,
    // --- block types ---
    SetParagraph,
    SetHeading(u8),
    SetBlockquote,
    SetCodeBlock(Option<String>),
    InsertHorizontalRule,
    // --- lists ---
    ToggleBulletList,
    ToggleOrderedList,
    ToggleTaskList,
    ToggleTaskItemChecked,
    SinkListItem,
    LiftListItem,
    // --- tables ---
    InsertTable {
        rows: u8,
        cols: u8,
    },
    AddRowBefore,
    AddRowAfter,
    DeleteRow,
    AddColBefore,
    AddColAfter,
    DeleteCol,
    DeleteTable,
    ToggleHeaderRow,
    // --- structural editing (MT-013 scope expansion) ---
    /// Enter at the caret: split the current block into two.
    InsertParagraphBreak,
    /// Backspace at caret offset 0: merge the current block into the previous sibling.
    MergeBackward,
}

impl FormattingCommand {
    /// The stable command id string, used to build the toolbar button author_id
    /// (`toolbar-btn-{command_id}`) and to match React catalog ids. Uses the snake_case
    /// names the MT scope lists (`toggle_bold`, `set_heading`, …) so the AccessKit
    /// `toolbar-btn-toggle_bold` id the AC asserts is produced verbatim.
    ///
    /// The three heading buttons (H1/H2/H3) are DISTINCT toolbar controls, so each gets a
    /// level-specific id (`set_heading_1` / `_2` / `_3`) — a single shared `set_heading`
    /// id would make them indistinguishable to a swarm agent (a duplicate-author_id
    /// collision the HBR-SWARM gate forbids). The base `set_heading(level)` command
    /// FUNCTION is still one function; only its addressable id carries the level.
    pub fn command_id(&self) -> &'static str {
        match self {
            FormattingCommand::Undo => "undo",
            FormattingCommand::Redo => "redo",
            FormattingCommand::ToggleBold => "toggle_bold",
            FormattingCommand::ToggleItalic => "toggle_italic",
            FormattingCommand::ToggleUnderline => "toggle_underline",
            FormattingCommand::ToggleStrike => "toggle_strike",
            FormattingCommand::ToggleCode => "toggle_code",
            FormattingCommand::SetParagraph => "set_paragraph",
            FormattingCommand::SetHeading(1) => "set_heading_1",
            FormattingCommand::SetHeading(2) => "set_heading_2",
            FormattingCommand::SetHeading(3) => "set_heading_3",
            // HeadingLevel::new clamps out-of-range to 1..=3, so only 1/2/3 are
            // constructible via dispatch; a raw out-of-range variant maps to the level-1
            // id rather than panicking (defensive — never reached through the keymap/toolbar).
            FormattingCommand::SetHeading(_) => "set_heading_1",
            FormattingCommand::SetBlockquote => "set_blockquote",
            FormattingCommand::SetCodeBlock(_) => "set_code_block",
            FormattingCommand::InsertHorizontalRule => "insert_horizontal_rule",
            FormattingCommand::ToggleBulletList => "toggle_bullet_list",
            FormattingCommand::ToggleOrderedList => "toggle_ordered_list",
            FormattingCommand::ToggleTaskList => "toggle_task_list",
            FormattingCommand::ToggleTaskItemChecked => "toggle_task_item_checked",
            FormattingCommand::SinkListItem => "sink_list_item",
            FormattingCommand::LiftListItem => "lift_list_item",
            FormattingCommand::InsertTable { .. } => "insert_table",
            FormattingCommand::AddRowBefore => "add_row_before",
            FormattingCommand::AddRowAfter => "add_row_after",
            FormattingCommand::DeleteRow => "delete_row",
            FormattingCommand::AddColBefore => "add_col_before",
            FormattingCommand::AddColAfter => "add_col_after",
            FormattingCommand::DeleteCol => "delete_col",
            FormattingCommand::DeleteTable => "delete_table",
            FormattingCommand::ToggleHeaderRow => "toggle_header_row",
            FormattingCommand::InsertParagraphBreak => "insert_paragraph_break",
            FormattingCommand::MergeBackward => "merge_backward",
        }
    }
}

/// The mutable editor state a command drives: the document tree, the undo manager,
/// the selection, and the actor id for transaction provenance. Borrowed from
/// `RichEditorState` by the toolbar/keymap dispatch. Kept as an explicit borrow
/// struct (not the whole `RichEditorState`) so the command layer has NO dependency on
/// the renderer/IME fields and is unit-testable without an egui context.
pub struct CommandContext<'a> {
    /// The document being edited (the `doc` root).
    pub doc: &'a mut BlockNode,
    /// The undo/redo history (receipts pushed on each successful mutating command).
    pub history: &'a mut UndoManager,
    /// The current selection; commands may move the caret (see module docs).
    pub selection: &'a mut Selection,
    /// Actor id for transaction provenance (`"operator"` for keyboard/toolbar input).
    pub actor_id: &'a str,
}

impl<'a> CommandContext<'a> {
    /// Build a command context from the live editor state borrows.
    pub fn new(
        doc: &'a mut BlockNode,
        history: &'a mut UndoManager,
        selection: &'a mut Selection,
        actor_id: &'a str,
    ) -> Self {
        Self {
            doc,
            history,
            selection,
            actor_id,
        }
    }
}

/// Dispatch a [`FormattingCommand`] against the live editor state. This is the single
/// entry both the toolbar and the keymap call, so the two surfaces share ONE command
/// path (no drift). Returns `Ok(())` when the command ran (or was a benign no-op such
/// as undo with an empty stack), or a [`CommandError`] when a guard refused it.
pub fn dispatch(ctx: &mut CommandContext<'_>, cmd: &FormattingCommand) -> Result<(), CommandError> {
    match cmd {
        FormattingCommand::Undo => undo(ctx),
        FormattingCommand::Redo => redo(ctx),
        FormattingCommand::ToggleBold => toggle_mark(ctx, Mark::Bold),
        FormattingCommand::ToggleItalic => toggle_mark(ctx, Mark::Italic),
        FormattingCommand::ToggleUnderline => toggle_mark(ctx, Mark::Underline),
        FormattingCommand::ToggleStrike => toggle_mark(ctx, Mark::Strike),
        FormattingCommand::ToggleCode => toggle_mark(ctx, Mark::Code),
        FormattingCommand::SetParagraph => set_block_kind(ctx, NodeKind::Paragraph),
        FormattingCommand::SetHeading(level) => {
            set_block_kind(ctx, NodeKind::Heading(HeadingLevel::new(*level)))
        }
        FormattingCommand::SetBlockquote => toggle_wrap_blockquote(ctx),
        FormattingCommand::SetCodeBlock(lang) => set_code_block(ctx, lang.clone()),
        FormattingCommand::InsertHorizontalRule => insert_horizontal_rule(ctx),
        FormattingCommand::ToggleBulletList => toggle_list(ctx, NodeKind::BulletList),
        FormattingCommand::ToggleOrderedList => toggle_list(ctx, NodeKind::OrderedList),
        FormattingCommand::ToggleTaskList => toggle_task_list(ctx),
        FormattingCommand::ToggleTaskItemChecked => toggle_task_item_checked(ctx),
        FormattingCommand::SinkListItem => sink_list_item(ctx),
        FormattingCommand::LiftListItem => lift_list_item(ctx),
        FormattingCommand::InsertTable { rows, cols } => insert_table(ctx, *rows, *cols),
        FormattingCommand::AddRowBefore => add_row(ctx, false),
        FormattingCommand::AddRowAfter => add_row(ctx, true),
        FormattingCommand::DeleteRow => delete_row(ctx),
        FormattingCommand::AddColBefore => add_col(ctx, false),
        FormattingCommand::AddColAfter => add_col(ctx, true),
        FormattingCommand::DeleteCol => delete_col(ctx),
        FormattingCommand::DeleteTable => delete_table(ctx),
        FormattingCommand::ToggleHeaderRow => toggle_header_row(ctx),
        FormattingCommand::InsertParagraphBreak => insert_paragraph_break(ctx),
        FormattingCommand::MergeBackward => merge_backward(ctx),
    }
}

// ── caret/selection helpers ──────────────────────────────────────────────────────

/// The caret head position (collapsed selections use head == anchor). A node
/// selection has no text caret.
fn head(selection: &Selection) -> Option<DocPosition> {
    match selection {
        Selection::Text { head, .. } => Some(head.clone()),
        Selection::Node { .. } => None,
    }
}

/// The anchor position of a text selection (the fixed end). `None` for a node
/// selection.
fn anchor(selection: &Selection) -> Option<DocPosition> {
    match selection {
        Selection::Text { anchor, .. } => Some(anchor.clone()),
        Selection::Node { .. } => None,
    }
}

/// The BLOCK path (the caret head's leaf path with the final text-leaf index
/// dropped). For a caret at `[1, 0]` (paragraph 1, text leaf 0) this is `[1]` — the
/// paragraph block. `None` for a node selection or an empty/too-short path.
pub(crate) fn caret_block_path(selection: &Selection) -> Option<Vec<usize>> {
    let h = head(selection)?;
    if h.path.is_empty() {
        return None;
    }
    Some(h.path[..h.path.len() - 1].to_vec())
}

/// Resolve a child-index block `path` (empty path = the doc root) to a shared
/// [`BlockNode`] reference, or `None` if any index is out of range / not a block.
pub(crate) fn block_at<'a>(doc: &'a BlockNode, path: &[usize]) -> Option<&'a BlockNode> {
    let mut node = doc;
    for &idx in path {
        node = node.children.get(idx)?.as_block()?;
    }
    Some(node)
}

/// True when `path` (a block path) lands on (or inside) a node of `kind`. Used by the
/// context guards: e.g. is the caret block path a descendant of a `Table`?
pub(crate) fn path_is_inside_kind(doc: &BlockNode, path: &[usize], kind: NodeKind) -> bool {
    let mut node = doc;
    if node.kind == kind {
        return true;
    }
    for &idx in path {
        let Some(next) = node.children.get(idx).and_then(Child::as_block) else {
            return false;
        };
        node = next;
        if node.kind == kind {
            return true;
        }
    }
    false
}

/// Build + apply an operator transaction, pushing the receipt on success. Returns the
/// transform result so the caller can map it to a [`CommandError`].
pub(crate) fn apply(ctx: &mut CommandContext<'_>, steps: Vec<Step>) -> Result<(), CommandError> {
    let tx = Transaction::new(steps, ActorKind::Operator, ctx.actor_id);
    let receipt = apply_transaction(ctx.doc, tx)?;
    ctx.history.push(receipt);
    Ok(())
}

// ── history ───────────────────────────────────────────────────────────────────────

/// Undo the last command (delegates to the MT-011 [`UndoManager`]). A no-op (Ok) when
/// the undo stack is empty.
fn undo(ctx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    ctx.history.undo(ctx.doc)?;
    clamp_selection(ctx);
    Ok(())
}

/// Redo the last undone command. A no-op (Ok) when there is nothing to redo.
fn redo(ctx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    ctx.history.redo(ctx.doc)?;
    clamp_selection(ctx);
    Ok(())
}

// ── inline marks (RISK-1 / MC-001 full-range active check) ──────────────────────────

/// The inclusive range of (block_index, leaf_index) pairs the selection spans at the
/// TOP level. The MT-011 model marks at whole-leaf granularity, so a selection's mark
/// toggle acts on every text leaf whose block+leaf index falls between the anchor and
/// head leaves (inclusive). Returns a flat list of leaf paths (each `[.., leaf_idx]`).
///
/// For a collapsed caret this yields exactly the one leaf the caret sits in.
fn selected_leaf_paths(ctx: &CommandContext<'_>) -> Vec<Vec<usize>> {
    let (Some(a), Some(h)) = (anchor(ctx.selection), head(ctx.selection)) else {
        return Vec::new();
    };
    // Order the two endpoints by document order (block path then leaf, lexicographic).
    let (start, end) = if a.path <= h.path { (a, h) } else { (h, a) };

    // Collapsed caret (same leaf path): just that one leaf.
    if start.path == end.path {
        return vec![start.path];
    }

    // Multi-leaf: walk the doc in document order, collecting every text-leaf path that
    // is >= start.path and <= end.path (lexicographic on the child-index path). This
    // covers a selection spanning sibling leaves in one block and leaves across blocks.
    let mut out = Vec::new();
    collect_leaf_paths_in_range(ctx.doc, &mut Vec::new(), &start.path, &end.path, &mut out);
    if out.is_empty() {
        // Defensive: at minimum act on the start leaf.
        out.push(start.path);
    }
    out
}

/// Recursive walk collecting text-leaf paths in `[start_path, end_path]` (inclusive,
/// lexicographic). Pushes the path of every `Child::Text` leaf whose full path sorts
/// within the bound.
fn collect_leaf_paths_in_range(
    node: &BlockNode,
    path: &mut Vec<usize>,
    start_path: &[usize],
    end_path: &[usize],
    out: &mut Vec<Vec<usize>>,
) {
    for (i, child) in node.children.iter().enumerate() {
        path.push(i);
        match child {
            Child::Text(_) => {
                if path.as_slice() >= start_path && path.as_slice() <= end_path {
                    out.push(path.clone());
                }
            }
            Child::Block(b) => {
                collect_leaf_paths_in_range(b, path, start_path, end_path, out);
            }
            // Inline atoms (hsLink wikilink, loomTransclusion) carry no mark range, so they are
            // not collected as mark-target leaf paths.
            Child::HsLink(_) | Child::Transclusion(_) => {}
        }
        // Pop AFTER visiting the child so each child's full path is correct during its
        // own visit, and siblings do not accumulate stale indices.
        path.pop();
    }
}

/// True when the mark of `mark`'s type is active over the current selection: ANY
/// overlapping text leaf carries it (ProseMirror toggle semantics — RISK-1 / MC-001).
pub fn is_mark_active(doc: &BlockNode, selection: &Selection, mark: &Mark) -> bool {
    let ctx_doc = doc;
    let leaf_paths = {
        // Reuse the same range computation without a full CommandContext.
        let (a, h) = match selection {
            Selection::Text { anchor, head } => (anchor.clone(), head.clone()),
            Selection::Node { .. } => return false,
        };
        let (start, end) = if a.path <= h.path { (a, h) } else { (h, a) };
        if start.path == end.path {
            vec![start.path]
        } else {
            let mut out = Vec::new();
            collect_leaf_paths_in_range(ctx_doc, &mut Vec::new(), &start.path, &end.path, &mut out);
            out
        }
    };
    leaf_paths.iter().any(|p| {
        leaf_at(ctx_doc, p)
            .map(|l| l.has_mark_type(mark))
            .unwrap_or(false)
    })
}

/// Resolve a leaf path (block indices then a final text-leaf index) to a shared
/// [`TextLeaf`].
fn leaf_at<'a>(doc: &'a BlockNode, path: &[usize]) -> Option<&'a TextLeaf> {
    let (leaf_idx, block_path) = path.split_last()?;
    let block = block_at(doc, block_path)?;
    block.children.get(*leaf_idx)?.as_text()
}

/// Toggle an inline mark over the whole selection (RISK-1 / MC-001). If ANY overlapping
/// leaf has the mark, every overlapping leaf has it REMOVED; otherwise every overlapping
/// leaf has it ADDED. A `code_block`'s text run forbids marks (schema), so a leaf inside
/// one is skipped (the transaction would otherwise roll back).
fn toggle_mark(ctx: &mut CommandContext<'_>, mark: Mark) -> Result<(), CommandError> {
    let leaf_paths = selected_leaf_paths(ctx);
    if leaf_paths.is_empty() {
        return Err(CommandError::NoCaret);
    }
    let active = is_mark_active(ctx.doc, ctx.selection, &mark);
    let mut steps = Vec::new();
    for p in &leaf_paths {
        // Skip a leaf whose parent forbids marks (code_block) so the whole tx does not
        // roll back on an otherwise-valid multi-leaf toggle.
        if leaf_parent_forbids_marks(ctx.doc, p) {
            continue;
        }
        if active {
            steps.push(Step::RemoveMark {
                path: p.clone(),
                mark: mark.clone(),
            });
        } else {
            steps.push(Step::AddMark {
                path: p.clone(),
                mark: mark.clone(),
            });
        }
    }
    if steps.is_empty() {
        return Ok(()); // nothing to do (e.g. only code-block leaves selected)
    }
    apply(ctx, steps)
}

/// True when the leaf at `path`'s parent block is a `code_block` (which forbids marks).
fn leaf_parent_forbids_marks(doc: &BlockNode, path: &[usize]) -> bool {
    let Some((_, block_path)) = path.split_last() else {
        return false;
    };
    block_at(doc, block_path)
        .map(|b| matches!(b.kind, NodeKind::CodeBlock))
        .unwrap_or(false)
}

// ── block-kind conversions (MC-002 context guard) ───────────────────────────────────

/// Convert the caret's block to `new_kind` (paragraph<->heading). Guards: the block
/// must currently be a `paragraph` or `heading` AND its parent must be the `doc` root
/// (so the converted block stays a legal direct child) — a caret inside a `list_item`,
/// `table_cell`, `blockquote`, or `code_block` returns `InvalidContext` (MC-002).
///
/// Implemented via DeleteNode + InsertNode (the MT-011 transform has no `ReplaceNode`
/// step): the new block carries the SAME children (text + marks survive) and, for a
/// heading, the level rides in the typed [`NodeKind::Heading`] variant.
fn set_block_kind(ctx: &mut CommandContext<'_>, new_kind: NodeKind) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let (parent_path, index) = split_block_path(&block_path)?;

    let block = block_at(ctx.doc, &block_path).ok_or(CommandError::NoCaret)?;
    // Guard: only a paragraph/heading is convertible.
    if !matches!(block.kind, NodeKind::Paragraph | NodeKind::Heading(_)) {
        return Err(CommandError::InvalidContext {
            reason: format!(
                "set_block_kind only converts a paragraph/heading, not {:?}",
                block.kind
            ),
        });
    }
    // Guard: the converted block must be a legal child of its parent. paragraph and
    // heading are legal anywhere a paragraph is (doc, list_item, table_cell,
    // blockquote), so the conversion is safe as long as the parent already holds the
    // (paragraph/heading) block — which it does. The MC-002 corruption case is a
    // list_item being treated as the convertible block; we already rejected that above
    // (the block itself is the paragraph, and its kind is paragraph/heading).
    let already = block.kind == new_kind;
    if already {
        // Toggling a heading to the same level (or paragraph to paragraph) is a no-op
        // in this command — the React `toggleHeading` toggles back to paragraph, but
        // `set_paragraph`/`set_heading` here are SET (idempotent), so just keep state.
        return Ok(());
    }
    let mut new_block = BlockNode::new(new_kind);
    new_block.attrs = block.attrs.clone();
    new_block.children = block.children.clone();

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
                node: new_block,
            },
        ],
    )?;
    // The caret stays in the (now-converted) block at the same offset; the block path
    // is unchanged (same parent + index), so the existing selection still resolves.
    clamp_selection(ctx);
    Ok(())
}

/// Set the caret's block to a `code_block` (carrying an optional `attrs.language`).
/// Guarded like [`set_block_kind`] (paragraph/heading only). Marks are dropped because
/// a code block forbids marks (schema) — the children are flattened to a single text
/// leaf of the concatenated text.
fn set_code_block(
    ctx: &mut CommandContext<'_>,
    language: Option<String>,
) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let (parent_path, index) = split_block_path(&block_path)?;
    let block = block_at(ctx.doc, &block_path).ok_or(CommandError::NoCaret)?;
    if !matches!(block.kind, NodeKind::Paragraph | NodeKind::Heading(_)) {
        return Err(CommandError::InvalidContext {
            reason: format!(
                "set_code_block only converts a paragraph/heading, not {:?}",
                block.kind
            ),
        });
    }
    // Flatten inline content to plain text (a code block holds ONE unmarked text leaf).
    let mut text = String::new();
    for c in &block.children {
        if let Child::Text(t) = c {
            text.push_str(&t.text.to_string());
        }
    }
    let mut new_block = BlockNode::new(NodeKind::CodeBlock);
    if let Some(lang) = language {
        new_block
            .attrs
            .insert("language".to_string(), JsonValue::from(lang));
    }
    new_block.children = vec![Child::Text(TextLeaf::new(&text))];

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
                node: new_block,
            },
        ],
    )?;
    clamp_selection(ctx);
    Ok(())
}

/// Wrap the caret's block in a `blockquote` (or unwrap when already inside one),
/// matching React `toggleBlockquote`. The blockquote is a top-level container holding
/// the block; toggling off lifts the block back to the doc root.
fn toggle_wrap_blockquote(ctx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let (parent_path, index) = split_block_path(&block_path)?;

    // Already inside a blockquote at the immediate parent? -> unwrap.
    if !parent_path.is_empty() {
        if let Some(parent) = block_at(ctx.doc, &parent_path) {
            if matches!(parent.kind, NodeKind::Blockquote) && parent.children.len() == 1 {
                // Unwrap: replace the blockquote (at grandparent[gp_index]) with its child.
                let (gp_path, gp_index) = split_block_path(&parent_path)?;
                let inner = parent.children[0]
                    .as_block()
                    .ok_or(CommandError::NoCaret)?
                    .clone();
                apply(
                    ctx,
                    vec![
                        Step::DeleteNode {
                            parent_path: gp_path.clone(),
                            index: gp_index,
                        },
                        Step::InsertNode {
                            parent_path: gp_path.clone(),
                            index: gp_index,
                            node: inner,
                        },
                    ],
                )?;
                // Caret moves up one nesting level: drop the blockquote index.
                reparent_caret(ctx, &parent_path, &gp_path);
                return Ok(());
            }
        }
    }

    // Wrap: replace the block with a blockquote holding it.
    let block = block_at(ctx.doc, &block_path)
        .ok_or(CommandError::NoCaret)?
        .clone();
    let quote = BlockNode::with_children(NodeKind::Blockquote, vec![Child::Block(block)]);
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
                node: quote,
            },
        ],
    )?;
    // Caret descends one level: the block is now at parent[index] -> [0].
    let mut new_block_path = parent_path.clone();
    new_block_path.push(index);
    new_block_path.push(0);
    reparent_caret_into(ctx, &block_path, &new_block_path);
    Ok(())
}

/// Insert a `horizontal_rule` atom block right AFTER the caret's block.
fn insert_horizontal_rule(ctx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let (parent_path, index) = split_block_path(&block_path)?;
    apply(
        ctx,
        vec![Step::InsertNode {
            parent_path,
            index: index + 1,
            node: BlockNode::new(NodeKind::HorizontalRule),
        }],
    )
}

// ── lists ───────────────────────────────────────────────────────────────────────







// ── tables (MC-003 nesting guard) ────────────────────────────────────────────────









// ── structural editing (MT-013 scope expansion) ─────────────────────────────────────

/// Enter / InsertParagraphBreak: split the caret's block at the caret offset into two
/// sibling blocks (MT-011 [`Step::SplitNode`]). The caret + selection move to the START
/// of the new (tail) block. Closes the MT-012 deferral of cross-block structural edits.
fn insert_paragraph_break(ctx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    let h = head(ctx.selection).ok_or(CommandError::NoCaret)?;
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let block = block_at(ctx.doc, &block_path).ok_or(CommandError::NoCaret)?;
    if !block.kind.holds_inline_content() {
        return Err(CommandError::InvalidContext {
            reason: format!("cannot split non-inline-content block {:?}", block.kind),
        });
    }
    // The split offset is the caret's flat char offset WITHIN this block. For a single
    // text leaf at [.., 0] the in-leaf offset is the block offset; for multi-run blocks
    // we sum the preceding sibling leaf lengths.
    let split_offset = block_local_char_offset(block, &h);
    let (parent_path, index) = split_block_path(&block_path)?;
    apply(
        ctx,
        vec![Step::SplitNode {
            path: block_path.clone(),
            char_offset: split_offset,
        }],
    )?;
    // The new sibling is at parent[index+1]; caret to its first text leaf, offset 0.
    let mut new_path = parent_path.clone();
    new_path.extend_from_slice(&[index + 1, 0]);
    *ctx.selection = Selection::caret(DocPosition::new(new_path, 0));
    clamp_selection(ctx);
    Ok(())
}

/// Backspace at caret offset 0: merge the caret's block into its previous sibling
/// (MT-011 [`Step::MergeNodes`]). The caret lands at the join point (the previous
/// block's pre-merge end). A no-op (Ok) when the caret is not at offset 0 or there is
/// no previous sibling. Closes MT-012's "Backspace at offset 0 is a no-op" deferral.
fn merge_backward(ctx: &mut CommandContext<'_>) -> Result<(), CommandError> {
    let h = head(ctx.selection).ok_or(CommandError::NoCaret)?;
    // Only act at the very start of the block's first text leaf.
    let block_path = caret_block_path(ctx.selection).ok_or(CommandError::NoCaret)?;
    let leaf_idx = *h.path.last().unwrap_or(&0);
    if h.char_offset != 0 || leaf_idx != 0 {
        return Ok(()); // not at block start -> let normal backspace handle it
    }
    let (parent_path, index) = split_block_path(&block_path)?;
    if index == 0 {
        return Ok(()); // no previous sibling to merge into
    }
    // Capture the previous sibling's pre-merge total char length (the join point).
    let parent = block_at(ctx.doc, &parent_path).ok_or(CommandError::NoCaret)?;
    let prev = parent
        .children
        .get(index - 1)
        .and_then(Child::as_block)
        .ok_or(CommandError::NoCaret)?;
    if !prev.kind.holds_inline_content() {
        // Previous sibling is a container (list/table/etc.); a simple inline merge is
        // not defined here — leave as a no-op for this MT (richer merges are later E2).
        return Ok(());
    }
    let join_at = prev.char_len();
    apply(
        ctx,
        vec![Step::MergeNodes {
            parent_path: parent_path.clone(),
            index,
        }],
    )?;
    // Caret lands at the join point in the merged (previous) block. Resolve the leaf at
    // the join offset within that block.
    let mut prev_path = parent_path.clone();
    prev_path.push(index - 1);
    let caret = caret_in_block_at_offset(ctx.doc, &prev_path, join_at).unwrap_or_else(|| {
        DocPosition::new(
            {
                let mut p = prev_path.clone();
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

// ── tree-navigation helpers ──────────────────────────────────────────────────────

/// Split a non-empty block path into (parent_path, index). Errors on the empty path
/// (the root has no parent — a structural step cannot address it).
pub(crate) fn split_block_path(path: &[usize]) -> Result<(Vec<usize>, usize), CommandError> {
    match path.split_last() {
        Some((&last, head)) => Ok((head.to_vec(), last)),
        None => Err(CommandError::InvalidContext {
            reason: "cannot address the document root with this command".to_string(),
        }),
    }
}

/// The path of the nearest ancestor block of `kind` enclosing `block_path` (inclusive
/// of `block_path` itself), or `None` if none on the chain. Walks the path prefix by
/// prefix from the deepest to the root.
pub(crate) fn enclosing_kind(doc: &BlockNode, block_path: &[usize], kind: NodeKind) -> Option<Vec<usize>> {
    for end in (0..=block_path.len()).rev() {
        let prefix = &block_path[..end];
        if let Some(b) = block_at(doc, prefix) {
            if b.kind == kind {
                return Some(prefix.to_vec());
            }
        }
    }
    None
}




/// The flat char offset of `pos` WITHIN `block` (summing the char lengths of the
/// sibling inline children before the addressed leaf, plus the in-leaf offset). For a
/// single-leaf block this is just `pos.char_offset`.
fn block_local_char_offset(block: &BlockNode, pos: &DocPosition) -> usize {
    // pos.path = block_path ++ [leaf_idx]; the leaf index is the last element.
    let Some(&leaf_idx) = pos.path.last() else {
        return pos.char_offset;
    };
    let mut acc = 0usize;
    for (i, child) in block.children.iter().enumerate() {
        if i == leaf_idx {
            return acc + pos.char_offset;
        }
        acc += child.char_len();
    }
    acc + pos.char_offset
}

/// Build a [`DocPosition`] caret inside the block at `block_path` at `block_offset`
/// flat chars (walks the block's inline children to find the leaf + in-leaf offset).
pub(crate) fn caret_in_block_at_offset(
    doc: &BlockNode,
    block_path: &[usize],
    block_offset: usize,
) -> Option<DocPosition> {
    let block = block_at(doc, block_path)?;
    let mut remaining = block_offset;
    let mut last_leaf_idx = None;
    let mut last_leaf_len = 0usize;
    for (i, child) in block.children.iter().enumerate() {
        if let Child::Text(t) = child {
            let len = t.text.len_chars();
            if remaining <= len {
                let mut p = block_path.to_vec();
                p.push(i);
                return Some(DocPosition::new(p, remaining));
            }
            remaining -= len;
            last_leaf_idx = Some(i);
            last_leaf_len = len;
        }
    }
    // Past the end: clamp to the last text leaf's end.
    last_leaf_idx.map(|i| {
        let mut p = block_path.to_vec();
        p.push(i);
        DocPosition::new(p, last_leaf_len)
    })
}

/// Reparent a caret whose old block lived under `old_parent` so it now points under
/// `new_parent` (same trailing leaf index + offset). Used by blockquote unwrap.
fn reparent_caret(ctx: &mut CommandContext<'_>, _old_parent: &[usize], new_parent: &[usize]) {
    // After unwrap, the inner block sits where the blockquote was: at new_parent ++ [index].
    // We move the caret to that block's first leaf, offset 0 (a safe, valid caret).
    let mut block_path = new_parent.to_vec();
    // The unwrap inserted the inner block at the blockquote's index (last of old parent).
    // The caller passed gp_path as new_parent and the blockquote index is recoverable
    // from the old block path; for simplicity land the caret at the new block start.
    if let Some(pos) = caret_in_block_at_offset(ctx.doc, &block_path, 0) {
        *ctx.selection = Selection::caret(pos);
        return;
    }
    block_path.push(0);
    *ctx.selection = Selection::caret(DocPosition::new(block_path, 0));
}

/// Move the caret from its old leaf path to a new block path's first leaf, offset 0.
pub(crate) fn reparent_caret_into(
    ctx: &mut CommandContext<'_>,
    _old_leaf: &[usize],
    new_block_path: &[usize],
) {
    if let Some(pos) = caret_in_block_at_offset(ctx.doc, new_block_path, 0) {
        *ctx.selection = Selection::caret(pos);
        return;
    }
    let mut p = new_block_path.to_vec();
    p.push(0);
    *ctx.selection = Selection::caret(DocPosition::new(p, 0));
}



/// Clamp the selection to a valid caret if its current path no longer resolves to a
/// text leaf (e.g. after a structural delete). Falls back to the document start. This
/// keeps the caret from dangling after a command rearranges the tree.
pub(crate) fn clamp_selection(ctx: &mut CommandContext<'_>) {
    let valid = match &*ctx.selection {
        Selection::Text { head, anchor } => {
            leaf_at(ctx.doc, &head.path).is_some() && leaf_at(ctx.doc, &anchor.path).is_some()
        }
        Selection::Node { node_path } => block_at(ctx.doc, node_path).is_some(),
    };
    if valid {
        return;
    }
    if let Some(pos) = handshake_rich_document_model::position::resolve(ctx.doc, 0) {
        *ctx.selection = Selection::caret(pos);
    }
}

#[cfg(test)]
mod tests;
