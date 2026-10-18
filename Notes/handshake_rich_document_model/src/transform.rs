//! Typed transform/step system for atomic document mutations (WP-KERNEL-012 MT-011).
//!
//! A [`Step`] is the smallest atomic edit; a [`Transaction`] is an ordered batch of
//! steps plus actor metadata. [`apply_transaction`] applies a transaction to a `doc`
//! with two hard invariants:
//!
//! 1. **Atomicity (red-team RISK-2):** the doc is CLONED before any step runs; if
//!    any step fails to apply OR the post-apply schema validation fails, the clone
//!    is restored and an `Err` is returned, so the doc never enters a corrupt
//!    intermediate state.
//! 2. **Invertibility (red-team RISK-4):** each step's inverse is computed from the
//!    OLD content captured BEFORE the step is applied, and collected into a
//!    [`TransactionReceipt`]. Re-applying the inverse steps in REVERSE order exactly
//!    restores the pre-transaction doc — the property the [`super::history`]
//!    undo manager relies on.
//!
//! All node addressing is by a `Vec<usize>` child-index path from the doc root
//! (the same path shape [`super::position::DocPosition`] uses); text addressing is
//! by CHAR range inside the leaf at the end of the path (never byte — RISK-1).

use thiserror::Error;

use super::node::{BlockNode, Child, Mark, NodeKind, TextLeaf};
use super::schema::{self, SchemaError};

/// Who/what produced a transaction. Threaded onto the [`Transaction`] so the event
/// ledger (HBR-INT) and swarm attribution (HBR-SWARM) can record provenance once
/// the editor wires transactions to the Flight Recorder in a later MT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorKind {
    /// A human operator editing in the GUI.
    Operator,
    /// An automated agent/model driving the editor via AccessKit/MCP.
    Agent,
    /// The system (migrations, programmatic load).
    System,
}

/// A single atomic document edit. Text steps address a CHAR range inside the text
/// leaf at `path`; structural steps address a node by `path`. The inverse of each
/// step is computed by `apply_transaction` from the old content, so a step does NOT
/// store its own inverse.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Insert `text` at `char_offset` inside the text leaf at `path`.
    InsertText {
        path: Vec<usize>,
        char_offset: usize,
        text: String,
    },
    /// Delete the `[start, end)` CHAR range from the text leaf at `path`.
    DeleteText {
        path: Vec<usize>,
        start: usize,
        end: usize,
    },
    /// Insert `node` as a child at `index` of the block node at `parent_path`.
    InsertNode {
        parent_path: Vec<usize>,
        index: usize,
        node: BlockNode,
    },
    /// Delete the child at `index` of the block node at `parent_path`.
    DeleteNode {
        parent_path: Vec<usize>,
        index: usize,
    },
    /// Split the text leaf inside the block at `path` at `char_offset`, producing a
    /// new sibling block of the SAME kind after it carrying the tail text. Used for
    /// "Enter splits a paragraph". `path` addresses the BLOCK (e.g. the paragraph),
    /// whose single text leaf is split.
    SplitNode {
        path: Vec<usize>,
        char_offset: usize,
    },
    /// Merge the block at `index` of `parent_path` into its previous sibling
    /// (appending the merged node's text leaf content). Used for "Backspace at start
    /// of paragraph merges into the previous one". Requires `index >= 1`.
    MergeNodes {
        parent_path: Vec<usize>,
        index: usize,
    },
    /// Add `mark` to the text leaf at `path` (whole-run mark — MT-011 models marks
    /// at run granularity; sub-run mark ranges come with the renderer split in a
    /// later MT). Replaces an existing mark of the same type.
    AddMark { path: Vec<usize>, mark: Mark },
    /// Remove every mark of `mark`'s type from the text leaf at `path`.
    RemoveMark { path: Vec<usize>, mark: Mark },
    /// Insert an INLINE child (`Text`, `HsLink`, or `Transclusion`) at `index` of the
    /// inline-content block at `parent_path`. MT-020 amendment (carried from MT-015): MT-011's
    /// [`Step::InsertNode`] only inserts a `Child::Block`, so inserting an inline atom (an `hsLink`
    /// wikilink, a `loomTransclusion`, or a styled text run) bypassed the transaction/undo system —
    /// an inline-atom insert was NOT undoable. This step makes inline-atom insertion go through
    /// `apply_transaction` (atomic + schema-validated + invertible), so a wikilink/transclusion/text
    /// insert is on the undo stack like every other edit. The inverse is a [`Step::DeleteInlineChild`].
    InsertInlineChild {
        parent_path: Vec<usize>,
        index: usize,
        child: Child,
    },
    /// Delete the INLINE child at `index` of the inline-content block at `parent_path`. The inverse
    /// is a [`Step::InsertInlineChild`] re-inserting the captured child, so an inline-atom delete is
    /// undoable (MT-020 amendment).
    DeleteInlineChild {
        parent_path: Vec<usize>,
        index: usize,
    },
}

/// An ordered batch of steps with actor metadata. Applied atomically.
#[derive(Debug, Clone, PartialEq)]
pub struct Transaction {
    /// The steps, applied in order.
    pub steps: Vec<Step>,
    /// Who produced the transaction.
    pub actor_kind: ActorKind,
    /// Stable actor id (operator id, agent author_id, …). Free-form; used for
    /// attribution, not addressing.
    pub actor_id: String,
}

impl Transaction {
    /// Build a transaction from steps and actor metadata.
    pub fn new(steps: Vec<Step>, actor_kind: ActorKind, actor_id: impl Into<String>) -> Self {
        Self {
            steps,
            actor_kind,
            actor_id: actor_id.into(),
        }
    }

    /// A convenience constructor for an operator transaction.
    pub fn operator(steps: Vec<Step>) -> Self {
        Self::new(steps, ActorKind::Operator, "operator")
    }
}

/// The result of applying a [`Transaction`]: the INVERSE steps needed to undo it
/// (in reverse-of-forward order, ready to apply as-is), plus the forward steps so
/// redo can re-apply. The [`super::history`] undo manager stores these.
#[derive(Debug, Clone, PartialEq)]
pub struct TransactionReceipt {
    /// The forward steps that were applied (for redo).
    pub forward: Vec<Step>,
    /// The inverse steps, ALREADY ordered so applying them in sequence undoes the
    /// transaction (i.e. reverse of the forward application order).
    pub inverse: Vec<Step>,
    /// Actor metadata copied from the transaction.
    pub actor_kind: ActorKind,
    /// Actor id copied from the transaction.
    pub actor_id: String,
}

/// Why a transaction could not be applied. A `Schema` error means the post-apply
/// tree was invalid and the doc was rolled back; an `Addressing` error means a step
/// pointed at a non-existent path/leaf/range and the doc was rolled back.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransformError {
    /// A step addressed a path that does not resolve to a node.
    #[error("no node at path {path:?}")]
    NoNodeAtPath { path: Vec<usize> },
    /// A step addressed a node that is not a text leaf where one was required.
    #[error("expected a text leaf at path {path:?}")]
    NotATextLeaf { path: Vec<usize> },
    /// A node-insert/delete index was out of range for the parent's children.
    #[error("child index {index} out of range for parent {parent:?} (len {len})")]
    ChildIndexOutOfRange {
        parent: NodeKind,
        index: usize,
        len: usize,
    },
    /// A split/merge precondition failed (e.g. merge at index 0, or split target is
    /// not an inline-content block with a text leaf).
    #[error("structural step invalid: {reason}")]
    InvalidStructuralStep { reason: String },
    /// The post-apply tree failed schema validation; the doc was rolled back.
    #[error("schema violation after apply: {0}")]
    Schema(#[from] SchemaError),
}

/// Apply `tx` to `doc` atomically. On success, `doc` is mutated in place and a
/// [`TransactionReceipt`] is returned. On ANY error, `doc` is left byte-for-byte
/// unchanged (it is restored from a clone taken before the first step) and the error
/// is returned.
///
/// The inverse of each step is captured from the OLD content BEFORE that step runs
/// (MT impl note 3 / RISK-4), then the inverse list is reversed so it applies as a
/// single undo batch.
pub fn apply_transaction(
    doc: &mut BlockNode,
    tx: Transaction,
) -> Result<TransactionReceipt, TransformError> {
    // RISK-2 atomicity: snapshot before touching anything.
    let snapshot = doc.clone();

    let mut inverse_in_apply_order: Vec<Step> = Vec::with_capacity(tx.steps.len());

    let result = (|| -> Result<(), TransformError> {
        for step in &tx.steps {
            // Capture the inverse from current (pre-step) state, THEN apply.
            let inverse = compute_inverse(doc, step)?;
            apply_step(doc, step)?;
            inverse_in_apply_order.push(inverse);
        }
        // Post-apply structural validation (RISK-2): a step batch that leaves the
        // tree invalid is rejected wholesale.
        schema::validate_tree(doc)?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            // Undo applies the inverses in reverse of the forward order.
            let mut inverse = inverse_in_apply_order;
            inverse.reverse();
            Ok(TransactionReceipt {
                forward: tx.steps,
                inverse,
                actor_kind: tx.actor_kind,
                actor_id: tx.actor_id,
            })
        }
        Err(e) => {
            // Roll back to the pre-transaction snapshot.
            *doc = snapshot;
            Err(e)
        }
    }
}

/// Compute the inverse of `step` against the CURRENT (pre-step) `doc`. The caller
/// guarantees this runs before `apply_step` for the same step, so the old content is
/// still present.
fn compute_inverse(doc: &BlockNode, step: &Step) -> Result<Step, TransformError> {
    match step {
        Step::InsertText {
            path,
            char_offset,
            text,
        } => {
            // Inverse of inserting `text` is deleting the char range it occupies.
            // `RopeText::insert` CLAMPS the offset into `0..=len_chars`, so the text
            // actually lands at `min(char_offset, len)` — the inverse must address
            // the CLAMPED start, not the raw `char_offset` (which may be usize::MAX
            // for an "append at end" insert and would overflow `start + n_chars`).
            let leaf = text_leaf_at(doc, path)?;
            let start = (*char_offset).min(leaf.text.len_chars());
            let n_chars = text.chars().count();
            Ok(Step::DeleteText {
                path: path.clone(),
                start,
                end: start + n_chars,
            })
        }
        Step::DeleteText { path, start, end } => {
            // Inverse of deleting [start,end) is re-inserting the deleted text.
            let leaf = text_leaf_at(doc, path)?;
            let removed = leaf.text.slice_chars(*start, *end);
            Ok(Step::InsertText {
                path: path.clone(),
                char_offset: *start,
                text: removed,
            })
        }
        Step::InsertNode {
            parent_path, index, ..
        } => {
            // Inverse of inserting a node is deleting it.
            Ok(Step::DeleteNode {
                parent_path: parent_path.clone(),
                index: *index,
            })
        }
        Step::DeleteNode { parent_path, index } => {
            // Inverse of deleting a node is re-inserting the captured node.
            let parent = block_at(doc, parent_path)?;
            let parent_kind = parent.kind;
            let parent_len = parent.children.len();
            let child =
                parent
                    .children
                    .get(*index)
                    .ok_or(TransformError::ChildIndexOutOfRange {
                        parent: parent_kind,
                        index: *index,
                        len: parent_len,
                    })?;
            let node = child
                .as_block()
                .ok_or_else(|| TransformError::InvalidStructuralStep {
                    reason: format!("child {index} of {:?} is text, not a block", parent.kind),
                })?
                .clone();
            Ok(Step::InsertNode {
                parent_path: parent_path.clone(),
                index: *index,
                node,
            })
        }
        Step::SplitNode { path, .. } => {
            // Splitting block at `path` creates a sibling at parent[index+1]; the
            // inverse merges that new sibling back into `path`.
            let (parent_path, index) = split_path(path)?;
            Ok(Step::MergeNodes {
                parent_path,
                index: index + 1,
            })
        }
        Step::MergeNodes { parent_path, index } => {
            // Merging child[index] into child[index-1] appends ALL of index's inline
            // content AFTER child[index-1]'s existing content; the inverse splits
            // child[index-1] at the join point, which is the pre-merge TOTAL flat
            // char length of child[index-1] (not just its first text leaf — RISK FIX
            // for multi-run nodes).
            let parent = block_at(doc, parent_path)?;
            if *index == 0 {
                return Err(TransformError::InvalidStructuralStep {
                    reason: "cannot merge the first child into a previous sibling".to_string(),
                });
            }
            let prev = parent
                .children
                .get(*index - 1)
                .and_then(Child::as_block)
                .ok_or_else(|| TransformError::InvalidStructuralStep {
                    reason: format!("previous sibling of index {index} is not a block"),
                })?;
            let join_at = prev.char_len();
            let mut prev_path = parent_path.clone();
            prev_path.push(*index - 1);
            Ok(Step::SplitNode {
                path: prev_path,
                char_offset: join_at,
            })
        }
        Step::AddMark { path, mark } => {
            // Inverse of adding a mark: if a mark of the same type already existed,
            // restoring it is the true inverse; but MT-011 models AddMark as
            // "set mark of this type", and the common case (no prior mark) inverts to
            // RemoveMark. We capture the displaced mark so undo restores it exactly.
            let leaf = text_leaf_at(doc, path)?;
            match leaf.marks.iter().find(|m| m.same_type(mark)) {
                Some(existing) => Ok(Step::AddMark {
                    path: path.clone(),
                    mark: existing.clone(),
                }),
                None => Ok(Step::RemoveMark {
                    path: path.clone(),
                    mark: mark.clone(),
                }),
            }
        }
        Step::RemoveMark { path, mark } => {
            // Inverse of removing a mark: re-add the exact mark that was present (so
            // a link's href survives the round-trip — RISK-3). If none was present,
            // the inverse is a no-op AddMark of the requested mark, which undo will
            // simply add; to keep undo lossless we capture the actual present mark.
            let leaf = text_leaf_at(doc, path)?;
            match leaf.marks.iter().find(|m| m.same_type(mark)) {
                Some(existing) => Ok(Step::AddMark {
                    path: path.clone(),
                    mark: existing.clone(),
                }),
                None => Ok(Step::RemoveMark {
                    path: path.clone(),
                    mark: mark.clone(),
                }),
            }
        }
        Step::InsertInlineChild {
            parent_path, index, ..
        } => {
            // Inverse of inserting an inline child is deleting it.
            Ok(Step::DeleteInlineChild {
                parent_path: parent_path.clone(),
                index: *index,
            })
        }
        Step::DeleteInlineChild { parent_path, index } => {
            // Inverse of deleting an inline child is re-inserting the captured child verbatim (so a
            // wikilink/transclusion/marked-text run restores exactly — undo losslessness).
            let parent = block_at(doc, parent_path)?;
            let parent_kind = parent.kind;
            let parent_len = parent.children.len();
            let child = parent
                .children
                .get(*index)
                .ok_or(TransformError::ChildIndexOutOfRange {
                    parent: parent_kind,
                    index: *index,
                    len: parent_len,
                })?
                .clone();
            Ok(Step::InsertInlineChild {
                parent_path: parent_path.clone(),
                index: *index,
                child,
            })
        }
    }
}

/// Apply `step` to `doc` in place. Pure mechanics — atomicity/validation are the
/// caller's job. Returns an addressing error if a path/index/range is invalid.
fn apply_step(doc: &mut BlockNode, step: &Step) -> Result<(), TransformError> {
    match step {
        Step::InsertText {
            path,
            char_offset,
            text,
        } => {
            let leaf = text_leaf_at_mut(doc, path)?;
            leaf.text.insert(*char_offset, text);
            Ok(())
        }
        Step::DeleteText { path, start, end } => {
            let leaf = text_leaf_at_mut(doc, path)?;
            leaf.text.remove(*start, *end);
            Ok(())
        }
        Step::InsertNode {
            parent_path,
            index,
            node,
        } => {
            let parent = block_at_mut(doc, parent_path)?;
            if *index > parent.children.len() {
                return Err(TransformError::ChildIndexOutOfRange {
                    parent: parent.kind,
                    index: *index,
                    len: parent.children.len(),
                });
            }
            parent.children.insert(*index, Child::Block(node.clone()));
            Ok(())
        }
        Step::DeleteNode { parent_path, index } => {
            let parent = block_at_mut(doc, parent_path)?;
            if *index >= parent.children.len() {
                return Err(TransformError::ChildIndexOutOfRange {
                    parent: parent.kind,
                    index: *index,
                    len: parent.children.len(),
                });
            }
            parent.children.remove(*index);
            Ok(())
        }
        Step::SplitNode { path, char_offset } => apply_split(doc, path, *char_offset),
        Step::MergeNodes { parent_path, index } => apply_merge(doc, parent_path, *index),
        Step::AddMark { path, mark } => {
            let leaf = text_leaf_at_mut(doc, path)?;
            leaf.add_mark(mark.clone());
            Ok(())
        }
        Step::RemoveMark { path, mark } => {
            let leaf = text_leaf_at_mut(doc, path)?;
            leaf.remove_marks_of_type(mark);
            Ok(())
        }
        Step::InsertInlineChild {
            parent_path,
            index,
            child,
        } => {
            let parent = block_at_mut(doc, parent_path)?;
            if *index > parent.children.len() {
                return Err(TransformError::ChildIndexOutOfRange {
                    parent: parent.kind,
                    index: *index,
                    len: parent.children.len(),
                });
            }
            parent.children.insert(*index, child.clone());
            Ok(())
        }
        Step::DeleteInlineChild { parent_path, index } => {
            let parent = block_at_mut(doc, parent_path)?;
            if *index >= parent.children.len() {
                return Err(TransformError::ChildIndexOutOfRange {
                    parent: parent.kind,
                    index: *index,
                    len: parent.children.len(),
                });
            }
            parent.children.remove(*index);
            Ok(())
        }
    }
}

/// Split the inline-content block at `path` at `char_offset`: the block keeps the
/// head inline content `[0, char_offset)`, and a new sibling of the SAME kind+attrs
/// holding the tail inline content `[char_offset, end)` is inserted right after it.
///
/// RISK FIX (multi-run): the WHOLE inline child sequence is split at the flat char
/// offset, not just `children.first()`. A text run straddling the offset is split
/// mid-run (each half keeps the run's marks); runs entirely before the offset stay in
/// the head, runs entirely after move to the tail; an `hsLink` atom (size 1) goes
/// wholesale to whichever side the offset falls on. This is the correct behaviour for
/// every styled paragraph (bold/italic/link runs, embedded wikilinks), not only the
/// single-run case.
fn apply_split(
    doc: &mut BlockNode,
    path: &[usize],
    char_offset: usize,
) -> Result<(), TransformError> {
    let (parent_path, index) = split_path(path)?;
    let target_kind;
    let target_attrs;
    let (head_children, tail_children) = {
        let target = block_at(doc, path)?;
        if !target.kind.holds_inline_content() {
            return Err(TransformError::InvalidStructuralStep {
                reason: format!("cannot split non-inline-content block {:?}", target.kind),
            });
        }
        target_kind = target.kind;
        target_attrs = target.attrs.clone();
        split_inline_children(&target.children, char_offset)
    };
    // Replace the target's children with the head, then insert the tail sibling.
    {
        let target = block_at_mut(doc, path)?;
        target.children = head_children;
    }
    let mut tail = BlockNode::new(target_kind);
    tail.attrs = target_attrs;
    tail.children = tail_children;
    let parent = block_at_mut(doc, &parent_path)?;
    parent.children.insert(index + 1, Child::Block(tail));
    Ok(())
}

/// Split a flat list of inline children (`Text` runs + `HsLink` atoms) at a flat
/// CHAR offset into (head, tail). A text run straddling the offset is split mid-run
/// with both halves keeping the run's marks; an atom (char_len 1) is placed wholly
/// in the head when the offset is past it, else in the tail. The head always ends
/// with at least one text leaf so the resulting block has addressable inline content
/// even when the split lands exactly on a boundary.
fn split_inline_children(children: &[Child], char_offset: usize) -> (Vec<Child>, Vec<Child>) {
    let mut head: Vec<Child> = Vec::new();
    let mut tail: Vec<Child> = Vec::new();
    let mut consumed = 0usize;
    for child in children {
        let len = child.char_len();
        if consumed >= char_offset {
            // Entirely after the split point.
            tail.push(child.clone());
        } else if consumed + len <= char_offset {
            // Entirely before the split point.
            head.push(child.clone());
        } else {
            // Straddles the split point — only a text run can be split mid-run; an
            // atom (len 1) is handled by the two branches above (consumed>=offset or
            // consumed+1<=offset), so this branch is reached only for a text leaf.
            match child {
                Child::Text(leaf) => {
                    let cut = char_offset - consumed;
                    let head_text = leaf.text.slice_chars(0, cut);
                    let tail_text = leaf.text.slice_chars(cut, len);
                    head.push(Child::Text(TextLeaf::with_marks(
                        &head_text,
                        leaf.marks.clone(),
                    )));
                    tail.push(Child::Text(TextLeaf::with_marks(
                        &tail_text,
                        leaf.marks.clone(),
                    )));
                }
                other => {
                    // Defensive: a non-text child cannot straddle (len 1); place it in
                    // the tail so nothing is dropped.
                    tail.push(other.clone());
                }
            }
        }
        consumed += len;
    }
    // Guarantee each side has an addressable inline leaf (an empty paragraph still
    // holds one empty text leaf, matching the model's `BlockNode::paragraph("")`).
    if head.is_empty() {
        head.push(Child::Text(TextLeaf::new("")));
    }
    if tail.is_empty() {
        tail.push(Child::Text(TextLeaf::new("")));
    }
    (head, tail)
}

/// Merge child `index` of the block at `parent_path` into child `index-1` by
/// appending ALL of the merged node's children onto the previous node, then removing
/// the merged node.
///
/// RISK FIX (multi-run / dropped content): the previous implementation moved only the
/// merged node's FIRST text leaf, dropping any further runs, embedded `hsLink` atoms,
/// and block children. This now concatenates the FULL child sequence so a backspace-
/// merge of a styled paragraph keeps every run, mark, and inline atom. When both
/// sides are inline-content blocks, the boundary leaves are joined into one run only
/// when their marks match (so a bold tail does not silently lose its boldness); else
/// the merged children are appended as-is.
fn apply_merge(
    doc: &mut BlockNode,
    parent_path: &[usize],
    index: usize,
) -> Result<(), TransformError> {
    if index == 0 {
        return Err(TransformError::InvalidStructuralStep {
            reason: "cannot merge the first child into a previous sibling".to_string(),
        });
    }
    // Take the merged node's full child list first (immutable read + clone).
    let merged_children = {
        let parent = block_at(doc, parent_path)?;
        let merged = parent
            .children
            .get(index)
            .and_then(Child::as_block)
            .ok_or_else(|| TransformError::InvalidStructuralStep {
                reason: format!("child {index} is not a block to merge"),
            })?;
        merged.children.clone()
    };
    // Append the full sequence to the previous sibling.
    {
        let parent = block_at_mut(doc, parent_path)?;
        let prev = parent
            .children
            .get_mut(index - 1)
            .and_then(Child::as_block_mut)
            .ok_or_else(|| TransformError::InvalidStructuralStep {
                reason: format!("previous sibling of index {index} is not a block"),
            })?;
        append_inline_children(prev, merged_children);
    }
    // Remove the merged node.
    let parent = block_at_mut(doc, parent_path)?;
    if index >= parent.children.len() {
        return Err(TransformError::ChildIndexOutOfRange {
            parent: parent.kind,
            index,
            len: parent.children.len(),
        });
    }
    parent.children.remove(index);
    Ok(())
}

/// Append `incoming` children onto `prev`, coalescing the join boundary only when
/// both `prev`'s last child and `incoming`'s first child are text leaves with EQUAL
/// marks (so "hello" + "world" becomes one "helloworld" run, but a bold tail stays a
/// separate bold run). All other children — additional runs, `hsLink` atoms, block
/// children — are pushed verbatim so nothing is dropped.
fn append_inline_children(prev: &mut BlockNode, incoming: Vec<Child>) {
    let mut iter = incoming.into_iter();
    if let Some(first) = iter.next() {
        match (prev.children.last_mut(), &first) {
            (Some(Child::Text(prev_leaf)), Child::Text(in_leaf))
                if prev_leaf.marks == in_leaf.marks =>
            {
                let at = prev_leaf.text.len_chars();
                prev_leaf.text.insert(at, &in_leaf.text.to_string());
            }
            _ => prev.children.push(first),
        }
        for child in iter {
            prev.children.push(child);
        }
    }
}

/// Split a node path into (parent_path, index_within_parent). Errors on the empty
/// path (the root has no parent).
fn split_path(path: &[usize]) -> Result<(Vec<usize>, usize), TransformError> {
    match path.split_last() {
        Some((&last, head)) => Ok((head.to_vec(), last)),
        None => Err(TransformError::InvalidStructuralStep {
            reason: "cannot address the root with a structural step".to_string(),
        }),
    }
}

/// Resolve a child-index `path` to a shared block reference. The empty path is the
/// root `doc` itself.
fn block_at<'a>(doc: &'a BlockNode, path: &[usize]) -> Result<&'a BlockNode, TransformError> {
    let mut node = doc;
    for &idx in path {
        node = node
            .children
            .get(idx)
            .and_then(Child::as_block)
            .ok_or_else(|| TransformError::NoNodeAtPath {
                path: path.to_vec(),
            })?;
    }
    Ok(node)
}

/// Resolve a child-index `path` to a mutable block reference. The empty path is the
/// root `doc` itself.
fn block_at_mut<'a>(
    doc: &'a mut BlockNode,
    path: &[usize],
) -> Result<&'a mut BlockNode, TransformError> {
    let mut node = doc;
    for &idx in path {
        node = node
            .children
            .get_mut(idx)
            .and_then(Child::as_block_mut)
            .ok_or_else(|| TransformError::NoNodeAtPath {
                path: path.to_vec(),
            })?;
    }
    Ok(node)
}

/// Resolve a `path` whose LAST element addresses a text leaf inside the block at the
/// preceding path, returning a shared reference to that leaf.
fn text_leaf_at<'a>(doc: &'a BlockNode, path: &[usize]) -> Result<&'a TextLeaf, TransformError> {
    let (parent_path, leaf_idx) = split_path(path).map_err(|_| TransformError::NotATextLeaf {
        path: path.to_vec(),
    })?;
    let parent = block_at(doc, &parent_path)?;
    parent
        .children
        .get(leaf_idx)
        .and_then(Child::as_text)
        .ok_or_else(|| TransformError::NotATextLeaf {
            path: path.to_vec(),
        })
}

/// Resolve a `path` whose LAST element addresses a text leaf, returning a mutable
/// reference to that leaf.
fn text_leaf_at_mut<'a>(
    doc: &'a mut BlockNode,
    path: &[usize],
) -> Result<&'a mut TextLeaf, TransformError> {
    let (parent_path, leaf_idx) = split_path(path).map_err(|_| TransformError::NotATextLeaf {
        path: path.to_vec(),
    })?;
    let parent = block_at_mut(doc, &parent_path)?;
    let parent_kind = parent.kind;
    let len = parent.children.len();
    parent
        .children
        .get_mut(leaf_idx)
        .ok_or(TransformError::ChildIndexOutOfRange {
            parent: parent_kind,
            index: leaf_idx,
            len,
        })?
        .as_text_mut()
        .ok_or_else(|| TransformError::NotATextLeaf {
            path: path.to_vec(),
        })
}

#[cfg(test)]
mod tests {
    use super::super::node::NodeKind;
    use super::*;

    fn doc_two_paras() -> BlockNode {
        BlockNode::doc(vec![
            BlockNode::paragraph("hello"),
            BlockNode::paragraph("world"),
        ])
    }

    #[test]
    fn insert_text_then_inverse_round_trips() {
        let mut doc = BlockNode::doc(vec![BlockNode::paragraph("hello")]);
        let before = doc.clone();
        let tx = Transaction::operator(vec![Step::InsertText {
            path: vec![0, 0],
            char_offset: 5,
            text: " world".to_string(),
        }]);
        let receipt = apply_transaction(&mut doc, tx).unwrap();
        assert_eq!(
            doc.children[0].as_block().unwrap().children[0]
                .as_text()
                .unwrap()
                .text
                .to_string(),
            "hello world"
        );
        // Apply the inverse to undo.
        let undo = Transaction::operator(receipt.inverse);
        apply_transaction(&mut doc, undo).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn schema_violation_rolls_back() {
        let mut doc = BlockNode::doc(vec![BlockNode::paragraph("hi")]);
        let before = doc.clone();
        // Insert a paragraph as a child of a paragraph's INLINE content -> invalid.
        let tx = Transaction::operator(vec![Step::InsertNode {
            parent_path: vec![0], // the paragraph
            index: 1,
            node: BlockNode::paragraph("nested"),
        }]);
        let err = apply_transaction(&mut doc, tx).unwrap_err();
        assert!(matches!(err, TransformError::Schema(_)));
        assert_eq!(doc, before, "doc must be unchanged after a rejected tx");
    }

    #[test]
    fn split_then_merge_inverse() {
        let mut doc = doc_two_paras();
        // Combine the two paras into "helloworld" via merge, then check inverse split.
        let before = doc.clone();
        let merge = Transaction::operator(vec![Step::MergeNodes {
            parent_path: vec![],
            index: 1,
        }]);
        let receipt = apply_transaction(&mut doc, merge).unwrap();
        // After merge: one paragraph "helloworld".
        assert_eq!(doc.children.len(), 1);
        assert_eq!(
            doc.children[0].as_block().unwrap().children[0]
                .as_text()
                .unwrap()
                .text
                .to_string(),
            "helloworld"
        );
        // Inverse restores the two paragraphs.
        apply_transaction(&mut doc, Transaction::operator(receipt.inverse)).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn insert_inline_atom_is_undoable() {
        // MT-020 amendment (carried MT-015): inserting an inline hsLink atom through
        // InsertInlineChild goes through apply_transaction, so its inverse (DeleteInlineChild)
        // restores the pre-insert doc exactly — the property the undo manager relies on.
        use super::super::node::HsLinkNode;
        let mut doc = BlockNode::doc(vec![BlockNode::paragraph("see ")]);
        let before = doc.clone();
        let tx = Transaction::operator(vec![Step::InsertInlineChild {
            parent_path: vec![0], // the paragraph
            index: 1,             // after the "see " text run
            child: Child::HsLink(HsLinkNode::new("wp", "WP-KERNEL-012", "the WP")),
        }]);
        let receipt = apply_transaction(&mut doc, tx).unwrap();
        // The atom landed as a sibling inline child of the text run.
        let para = doc.children[0].as_block().unwrap();
        assert_eq!(para.children.len(), 2);
        assert!(para.children[1].as_hs_link().is_some());
        // Undo restores the original (atom removed).
        apply_transaction(&mut doc, Transaction::operator(receipt.inverse)).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn delete_inline_atom_is_undoable_and_lossless() {
        // Deleting an inline atom and undoing restores the EXACT atom (payload intact).
        use super::super::node::{HsLinkNode, TransclusionNode};
        let mut para = BlockNode::new(NodeKind::Paragraph);
        para.children.push(Child::Text(TextLeaf::new("a")));
        para.children
            .push(Child::Transclusion(TransclusionNode::new("BLK-9")));
        para.children
            .push(Child::HsLink(HsLinkNode::new("note", "N-1", "note one")));
        let mut doc = BlockNode::doc(vec![para]);
        let before = doc.clone();
        // Delete the transclusion at inline index 1.
        let tx = Transaction::operator(vec![Step::DeleteInlineChild {
            parent_path: vec![0],
            index: 1,
        }]);
        let receipt = apply_transaction(&mut doc, tx).unwrap();
        assert_eq!(doc.children[0].as_block().unwrap().children.len(), 2);
        // Undo restores the transclusion in place.
        apply_transaction(&mut doc, Transaction::operator(receipt.inverse)).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn inline_atom_into_non_inline_block_rolls_back() {
        // Inserting an inline atom into the doc root (which holds only block children) must fail
        // schema validation and roll back atomically (RISK-2).
        use super::super::node::HsLinkNode;
        let mut doc = BlockNode::doc(vec![BlockNode::paragraph("x")]);
        let before = doc.clone();
        let tx = Transaction::operator(vec![Step::InsertInlineChild {
            parent_path: vec![], // the doc root — inline atoms are not allowed here
            index: 0,
            child: Child::HsLink(HsLinkNode::new("wp", "W", "w")),
        }]);
        let err = apply_transaction(&mut doc, tx).unwrap_err();
        assert!(matches!(err, TransformError::Schema(_)));
        assert_eq!(
            doc, before,
            "doc must be unchanged after the rejected inline insert"
        );
    }

    #[test]
    fn split_paragraph_creates_sibling() {
        let mut doc = BlockNode::doc(vec![BlockNode::paragraph("helloworld")]);
        let tx = Transaction::operator(vec![Step::SplitNode {
            path: vec![0],
            char_offset: 5,
        }]);
        apply_transaction(&mut doc, tx).unwrap();
        assert_eq!(doc.children.len(), 2);
        assert_eq!(
            doc.children[0].as_block().unwrap().children[0]
                .as_text()
                .unwrap()
                .text
                .to_string(),
            "hello"
        );
        assert_eq!(
            doc.children[1].as_block().unwrap().children[0]
                .as_text()
                .unwrap()
                .text
                .to_string(),
            "world"
        );
        assert_eq!(
            doc.children[1].as_block().unwrap().kind,
            NodeKind::Paragraph
        );
    }
}
