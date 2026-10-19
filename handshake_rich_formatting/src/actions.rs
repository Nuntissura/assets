//! Framework-independent document actions. The host supplies structural merge and grapheme policy.
use handshake_rich_document_model::history::UndoManager;
use handshake_rich_document_model::node::BlockNode;
use handshake_rich_document_model::position::DocPosition;
use handshake_rich_document_model::selection::Selection;
use handshake_rich_document_model::transform::{apply_transaction, Step, Transaction};

/// Host behavior used by the action engine. Offsets are Unicode scalar counts, never bytes.
/// Grapheme callbacks must return the previous/next whole cluster boundary within the text.
/// Merge must apply its transaction and record history itself, returning whether state changed.
pub trait ActionHost {
    fn merge_backward(&mut self, ctx: &mut EditContext<'_>) -> bool;
    fn prev_grapheme_boundary_chars(&self, text: &str, char_offset: usize) -> usize;
    fn next_grapheme_boundary_chars(&self, text: &str, char_offset: usize) -> usize;
}
/// The mutable editor state the input handler drives: the document tree, the current
/// selection, and the undo manager. Owned by the widget; passed by `&mut` so one input
/// pass can apply a batch of events in order.
pub struct EditContext<'a> {
    /// The document being edited (the `doc` root).
    pub doc: &'a mut BlockNode,
    /// The current selection (caret = collapsed selection).
    pub selection: &'a mut Selection,
    /// The undo/redo history.
    pub undo: &'a mut UndoManager,
    /// Actor id for transaction provenance (operator vs agent). The widget passes
    /// `"operator"` for keyboard input.
    pub actor_id: &'a str,
}

/// One decoded editor action, produced from an egui event. Kept as an explicit enum (not
/// applied inline) so the mapping is unit-testable WITHOUT a live egui context: a test
/// decodes a synthetic event list to `Vec<EditAction>` and asserts the mapping, and a
/// separate test applies actions to a doc and asserts the model result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditAction {
    /// Insert a string at the caret (a printable char or an IME commit chunk).
    Insert(String),
    /// Delete one char to the LEFT of the caret (Backspace).
    DeleteBackward,
    /// Delete one char to the left but do not perform the boundary merge. Used when the operator has
    /// rebound `merge_backward`: ordinary Backspace still deletes text, while the old structural
    /// command chord is no longer secretly active at offset zero.
    DeleteBackwardWithoutMerge,
    /// Delete one char to the RIGHT of the caret (Delete).
    DeleteForward,
    /// Move the caret one char left; `extend` keeps the anchor (Shift).
    MoveLeft { extend: bool },
    /// Move the caret one char right; `extend` keeps the anchor (Shift).
    MoveRight { extend: bool },
    /// Move the caret to the start of its leaf (Home).
    MoveHome { extend: bool },
    /// Move the caret to the end of its leaf (End).
    MoveEnd { extend: bool },
    /// Select the whole current leaf (Ctrl+A, vertical-slice scope).
    SelectAll,
    /// Undo the last transaction (Ctrl+Z).
    Undo,
    /// Redo (Ctrl+Shift+Z).
    Redo,
}

/// Apply one [`EditAction`] to the edit context, returning `true` when the document or
/// selection changed. Text mutations go through [`apply_transaction`] and are pushed onto
/// the undo manager; caret moves only update the selection.
pub fn apply_action(ctx: &mut EditContext<'_>, action: EditAction, host: &mut dyn ActionHost) -> bool {
    match action {
        EditAction::Insert(text) => insert_text(ctx, &text),
        EditAction::DeleteBackward => delete(ctx, true, true, host),
        EditAction::DeleteBackwardWithoutMerge => delete(ctx, true, false, host),
        EditAction::DeleteForward => delete(ctx, false, false, host),
        EditAction::MoveLeft { extend } => move_horizontal(ctx, -1, extend, host),
        EditAction::MoveRight { extend } => move_horizontal(ctx, 1, extend, host),
        EditAction::MoveHome { extend } => move_boundary(ctx, true, extend),
        EditAction::MoveEnd { extend } => move_boundary(ctx, false, extend),
        EditAction::SelectAll => select_all(ctx),
        EditAction::Undo => ctx.undo.undo(ctx.doc).unwrap_or(false),
        EditAction::Redo => ctx.undo.redo(ctx.doc).unwrap_or(false),
    }
}

/// The caret head position (collapsed selections use head==anchor). For a node selection
/// (no text caret) we fall back to the doc start.
fn head(ctx: &EditContext<'_>) -> DocPosition {
    match &*ctx.selection {
        Selection::Text { head, .. } => head.clone(),
        Selection::Node { .. } => DocPosition::new(vec![0, 0], 0),
    }
}

/// The char length of the text leaf addressed by `pos.path` (its LAST element addresses
/// the leaf). Returns 0 when the path does not resolve to a text leaf (defensive — the
/// caret-bound clamp then keeps offsets at 0).
fn leaf_len(doc: &BlockNode, pos: &DocPosition) -> usize {
    resolve_leaf(doc, &pos.path)
        .map(|l| l.text.len_chars())
        .unwrap_or(0)
}

/// Resolve a `path` (block child indices then a final text-leaf index) to a shared
/// [`handshake_rich_document_model::node::TextLeaf`] reference.
fn resolve_leaf<'a>(
    doc: &'a BlockNode,
    path: &[usize],
) -> Option<&'a handshake_rich_document_model::node::TextLeaf> {
    let (leaf_idx, block_path) = path.split_last()?;
    let mut node = doc;
    for &idx in block_path {
        node = node.children.get(idx)?.as_block()?;
    }
    node.children.get(*leaf_idx)?.as_text()
}

/// Insert `text` at the caret, then move the caret to just after the inserted text. The
/// offset is validated against the leaf length at this layer (caret-bound discipline).
fn insert_text(ctx: &mut EditContext<'_>, text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let pos = head(ctx);
    let len = leaf_len(ctx.doc, &pos);
    let offset = pos.char_offset.min(len);
    let tx = Transaction::new(
        vec![Step::InsertText {
            path: pos.path.clone(),
            char_offset: offset,
            text: text.to_string(),
        }],
        handshake_rich_document_model::transform::ActorKind::Operator,
        ctx.actor_id,
    );
    match apply_transaction(ctx.doc, tx) {
        Ok(receipt) => {
            ctx.undo.push(receipt);
            let new_off = offset + text.chars().count();
            *ctx.selection = Selection::caret(DocPosition::new(pos.path, new_off));
            true
        }
        Err(_) => false,
    }
}

/// Delete one char backward (Backspace) or forward (Delete) from the caret. A backward
/// delete at block offset 0 now routes to the MT-013 structural `merge_backward` command
/// (merge this block into the previous sibling) — closing MT-012's "Backspace at offset 0
/// is a no-op" deferral. A forward delete at the leaf end is still a no-op here (forward
/// cross-block merge is a later E2 pass).
fn delete(ctx: &mut EditContext<'_>, backward: bool, merge_at_boundary: bool, host: &mut dyn ActionHost) -> bool {
    let pos = head(ctx);
    let len = leaf_len(ctx.doc, &pos);
    let offset = pos.char_offset.min(len);
    let (start, end, new_off) = if backward {
        if offset == 0 {
            if !merge_at_boundary {
                return false;
            }
            // At the very start of the block's text -> structural merge into the previous
            // sibling (MT-013 scope expansion), via the command layer (which sets the
            // post-merge caret at the join point). A no-op when there is no previous
            // sibling / not at a block boundary.
            return host.merge_backward(ctx);
        }
        // MT-077 AC4: Backspace deletes the whole GRAPHEME CLUSTER before the caret (a family ZWJ
        // emoji / combining sequence / flag / Hangul syllable removes all its codepoints, not one).
        let prev = prev_grapheme_in_leaf(ctx.doc, &pos, offset, host);
        (prev, offset, prev)
    } else {
        if offset >= len {
            return false;
        }
        // MT-077 AC4: forward Delete removes the whole grapheme cluster after the caret.
        let next = next_grapheme_in_leaf(ctx.doc, &pos, offset, host);
        (offset, next, offset)
    };
    let tx = Transaction::new(
        vec![Step::DeleteText {
            path: pos.path.clone(),
            start,
            end,
        }],
        handshake_rich_document_model::transform::ActorKind::Operator,
        ctx.actor_id,
    );
    match apply_transaction(ctx.doc, tx) {
        Ok(receipt) => {
            ctx.undo.push(receipt);
            *ctx.selection = Selection::caret(DocPosition::new(pos.path, new_off));
            true
        }
        Err(_) => false,
    }
}

/// Move the caret horizontally one GRAPHEME CLUSTER within its leaf, clamped to `0..=leaf_len`. `delta`
/// carries only the DIRECTION (negative = left, positive = right); the step size is one grapheme cluster
/// (MT-077 AC3) so a family ZWJ emoji / combining sequence / flag / Hangul syllable is crossed whole, not
/// one codepoint at a time. `extend` keeps the anchor (Shift+Arrow builds a range); otherwise the
/// selection collapses to the new head. Cross-leaf motion is out of the vertical-slice scope: the caret
/// clamps at the leaf boundary.
fn move_horizontal(ctx: &mut EditContext<'_>, delta: i64, extend: bool, host: &dyn ActionHost) -> bool {
    let pos = head(ctx);
    let len = leaf_len(ctx.doc, &pos);
    let cur = pos.char_offset.min(len);
    let next = if delta < 0 {
        prev_grapheme_in_leaf(ctx.doc, &pos, cur, host)
    } else if delta > 0 {
        next_grapheme_in_leaf(ctx.doc, &pos, cur, host)
    } else {
        cur
    };
    if next == pos.char_offset && !extend {
        // Already collapsed at the boundary; nothing visible changes.
        if matches!(ctx.selection, Selection::Text { anchor, head } if anchor == head) {
            return false;
        }
    }
    set_head(ctx, DocPosition::new(pos.path, next), extend);
    true
}

/// The CHAR offset of the previous grapheme-cluster boundary `< char_offset` within the text leaf
/// addressed by `pos` (MT-077 AC3/AC4). Resolves the leaf text and delegates to the shared
/// `text_intl::grapheme` module in CHAR units (the rich editor's rope is char-addressed). A leaf that
/// does not resolve, or an offset of 0, returns the floor (0 / a single char step) so the caret never
/// stalls. The leaf text is bounded (one inline run), so materializing it is cheap; the shared module
/// additionally windows the segmentation for very long leaves (RISK-1).
fn prev_grapheme_in_leaf(doc: &BlockNode, pos: &DocPosition, char_offset: usize, host: &dyn ActionHost) -> usize {
    if char_offset == 0 {
        return 0;
    }
    match resolve_leaf(doc, &pos.path) {
        Some(leaf) => {
            let text = leaf.text.to_string();
            host.prev_grapheme_boundary_chars(&text, char_offset)
        }
        // No resolvable leaf: fall back to a single char step so Backspace/LEFT still moves.
        None => char_offset.saturating_sub(1),
    }
}

/// The CHAR offset of the next grapheme-cluster boundary `> char_offset` within the addressed text leaf
/// (MT-077 AC3/AC4). Char-unit sibling of [`prev_grapheme_in_leaf`].
fn next_grapheme_in_leaf(doc: &BlockNode, pos: &DocPosition, char_offset: usize, host: &dyn ActionHost) -> usize {
    match resolve_leaf(doc, &pos.path) {
        Some(leaf) => {
            let text = leaf.text.to_string();
            let len = text.chars().count();
            if char_offset >= len {
                return len;
            }
            host.next_grapheme_boundary_chars(&text, char_offset)
        }
        None => char_offset + 1,
    }
}

/// Move the caret to the start (`to_start`) or end of its leaf (Home/End in the
/// single-paragraph vertical slice = leaf boundary). `extend` keeps the anchor.
fn move_boundary(ctx: &mut EditContext<'_>, to_start: bool, extend: bool) -> bool {
    let pos = head(ctx);
    let len = leaf_len(ctx.doc, &pos);
    let target = if to_start { 0 } else { len };
    set_head(ctx, DocPosition::new(pos.path, target), extend);
    true
}

/// Select the whole current leaf (Ctrl+A vertical-slice scope): anchor at offset 0, head
/// at the leaf end.
fn select_all(ctx: &mut EditContext<'_>) -> bool {
    let pos = head(ctx);
    let len = leaf_len(ctx.doc, &pos);
    *ctx.selection = Selection::text(
        DocPosition::new(pos.path.clone(), 0),
        DocPosition::new(pos.path, len),
    );
    true
}

/// Set the caret head, either extending the current anchor (`extend`) or collapsing to a
/// caret at the new head.
fn set_head(ctx: &mut EditContext<'_>, new_head: DocPosition, extend: bool) {
    if extend {
        let anchor = match ctx.selection {
            Selection::Text { anchor, .. } => anchor.clone(),
            Selection::Node { .. } => new_head.clone(),
        };
        *ctx.selection = Selection::text(anchor, new_head);
    } else {
        *ctx.selection = Selection::caret(new_head);
    }
}

