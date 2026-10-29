//! Atomic replacement of a captured selection with resolved rich content.

use std::collections::HashMap;

use serde_json::Value;
use thiserror::Error;

use crate::{
    apply_transaction, apply_transaction_with_trace, schema, ActorKind, BlockNode, Child,
    DocPosition, Selection, Step, TextLeaf, Transaction, TransactionReceipt, TransactionTrace,
    TransformError,
};

/// Inline runs/atoms, or complete sibling blocks. Block fragments remain closed:
/// their first and last blocks are not implicitly merged into surrounding text.
#[derive(Debug, Clone, PartialEq)]
pub enum RichFragment {
    Inline(Vec<Child>),
    Blocks(Vec<BlockNode>),
}

/// Attribution and attributes for a new suffix block created by a block insertion.
#[derive(Debug, Clone, PartialEq)]
pub struct FragmentReplaceOptions {
    pub actor_kind: ActorKind,
    pub actor_id: String,
    /// Required when both sides of one attributed block survive a block insertion.
    /// The prefix retains the original attrs. The caller supplies this new block's
    /// complete attrs, including any fresh identity required by its own contract.
    pub split_tail_attrs: Option<HashMap<String, Value>>,
}

/// One applied transaction and the selection to install with that same host edit.
#[derive(Debug, Clone, PartialEq)]
pub struct FragmentReplacement {
    pub receipt: TransactionReceipt,
    pub selection: Selection,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TracedFragmentReplacement {
    pub receipt: TransactionReceipt,
    pub selection: Selection,
    pub trace: TransactionTrace,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum FragmentReplaceError {
    #[error("invalid selection position {0:?}")]
    InvalidPosition(DocPosition),
    #[error("invalid selected block path {0:?}")]
    InvalidNodePath(Vec<usize>),
    #[error("text endpoints must address inline blocks with the same parent")]
    CrossContainerRange,
    #[error("inline replacement across siblings requires identical block kinds and attrs")]
    IncompatibleInlineBoundaries,
    #[error("node selections require a block fragment")]
    InlineNodeSelection,
    #[error("a block fragment must contain at least one block")]
    EmptyBlockFragment,
    #[error("splitting an attributed block requires explicit new suffix attrs")]
    SplitTailAttributesRequired,
    #[error(transparent)]
    Transform(#[from] TransformError),
}

/// Replace a selection atomically and return one undo receipt. Input positions are
/// checked without clamping. Empty inline content deletes a text range. Node
/// selection replaces one whole block. Text ranges may span runs and sibling
/// inline blocks, including siblings inside a list item or table cell, but may not
/// cross their containing parents. Invalid input leaves `doc` unchanged.
/// An empty inline block admits the virtual child index 0 at offset 0; insertion
/// does not require a host-created text leaf. Other missing children are invalid.
///
/// The returned text selection is collapsed after the inserted content. A block
/// fragment whose final content has no text caret, including a trailing inline
/// atom or empty block, returns a node selection on its last block. Empty inline
/// content at a valid collapsed caret preserves the document and selection and
/// returns a zero-step receipt. The host owns history insertion, original async
/// edit context, revision/IME/fence checks, persistence, and undo/redo selection
/// restoration.
pub fn replace_selection_with_fragment(
    doc: &mut BlockNode,
    selection: &Selection,
    fragment: RichFragment,
    options: FragmentReplaceOptions,
) -> Result<FragmentReplacement, FragmentReplaceError> {
    replace_selection_with_fragment_inner(doc, selection, fragment, options, apply_transaction)
        .map(|(receipt, selection)| FragmentReplacement { receipt, selection })
}

/// Replace through the same fragment builder and one actual transaction, with
/// opt-in block-path lineage. Trace describes the emitted steps: replaced nodes
/// are removed even when a new node carries equal attributes. No prefix replay or
/// extra history entry is used. Errors leave the original document unchanged and
/// return no receipt, selection, or trace.
pub fn replace_selection_with_fragment_with_trace(
    doc: &mut BlockNode,
    selection: &Selection,
    fragment: RichFragment,
    options: FragmentReplaceOptions,
) -> Result<TracedFragmentReplacement, FragmentReplaceError> {
    replace_selection_with_fragment_inner(
        doc,
        selection,
        fragment,
        options,
        apply_transaction_with_trace,
    )
    .map(|(traced, selection)| TracedFragmentReplacement {
        receipt: traced.receipt,
        selection,
        trace: traced.trace,
    })
}

fn replace_selection_with_fragment_inner<R>(
    doc: &mut BlockNode,
    selection: &Selection,
    fragment: RichFragment,
    options: FragmentReplaceOptions,
    apply: impl FnOnce(&mut BlockNode, Transaction) -> Result<R, TransformError>,
) -> Result<(R, Selection), FragmentReplaceError> {
    schema::validate_tree(doc).map_err(TransformError::from)?;
    let (steps, selection) = match selection {
        Selection::Node { node_path } => {
            let RichFragment::Blocks(blocks) = fragment else {
                return Err(FragmentReplaceError::InlineNodeSelection);
            };
            validate_blocks(&blocks)?;
            let (&index, parent_path) = node_path
                .split_last()
                .ok_or_else(|| FragmentReplaceError::InvalidNodePath(node_path.clone()))?;
            block_at(doc, node_path)
                .ok_or_else(|| FragmentReplaceError::InvalidNodePath(node_path.clone()))?;
            let selection = block_end_selection(&blocks, parent_path, index);
            (replace_blocks(parent_path, index, index, blocks), selection)
        }
        Selection::Text { anchor, head } => {
            let (start, end) =
                if (&anchor.path, anchor.char_offset) <= (&head.path, head.char_offset) {
                    (anchor, head)
                } else {
                    (head, anchor)
                };
            let (start_path, start_block) = endpoint(doc, start)?;
            let (end_path, end_block) = endpoint(doc, end)?;
            let (&start_index, parent_path) = start_path
                .split_last()
                .ok_or(FragmentReplaceError::CrossContainerRange)?;
            let (&end_index, end_parent) = end_path
                .split_last()
                .ok_or(FragmentReplaceError::CrossContainerRange)?;
            if parent_path != end_parent {
                return Err(FragmentReplaceError::CrossContainerRange);
            }
            if start == end
                && matches!(&fragment, RichFragment::Inline(inline) if inline.is_empty())
            {
                let result = apply(
                    doc,
                    Transaction::new(Vec::new(), options.actor_kind, options.actor_id),
                )?;
                return Ok((result, selection.clone()));
            }
            let prefix = before(start_block, start);
            let suffix = after(end_block, end);
            match fragment {
                RichFragment::Inline(inline) => {
                    if start_block.kind != end_block.kind || start_block.attrs != end_block.attrs {
                        return Err(FragmentReplaceError::IncompatibleInlineBoundaries);
                    }
                    let mut children = prefix;
                    children.extend(inline);
                    // A text leaf at the seam represents a caret after an atom too.
                    if !matches!(children.last(), Some(Child::Text(_))) {
                        children.push(Child::Text(TextLeaf::new("")));
                    }
                    let caret_index = children.len() - 1;
                    let caret_offset = children[caret_index].char_len();
                    children.extend(suffix);
                    let mut caret_path = start_path.to_vec();
                    caret_path.push(caret_index);
                    let selection = Selection::caret(DocPosition::new(caret_path, caret_offset));
                    if start_path == end_path {
                        let mut steps: Vec<_> = (0..start_block.children.len())
                            .rev()
                            .map(|index| Step::DeleteInlineChild {
                                parent_path: start_path.to_vec(),
                                index,
                            })
                            .collect();
                        steps.extend(children.into_iter().enumerate().map(|(index, child)| {
                            Step::InsertInlineChild {
                                parent_path: start_path.to_vec(),
                                index,
                                child,
                            }
                        }));
                        (steps, selection)
                    } else {
                        let mut merged = start_block.clone();
                        merged.children = children;
                        (
                            replace_blocks(parent_path, start_index, end_index, vec![merged]),
                            selection,
                        )
                    }
                }
                RichFragment::Blocks(blocks) => {
                    validate_blocks(&blocks)?;
                    let mut replacement = Vec::new();
                    let has_prefix = !prefix.is_empty();
                    if has_prefix {
                        let mut block = start_block.clone();
                        block.children = prefix;
                        replacement.push(block);
                    }
                    let selection =
                        block_end_selection(&blocks, parent_path, start_index + replacement.len());
                    replacement.extend(blocks);
                    if !suffix.is_empty() {
                        let mut block = end_block.clone();
                        if start_path == end_path && has_prefix {
                            block.attrs = match options.split_tail_attrs {
                                Some(attrs) => attrs,
                                None if block.attrs.is_empty() => HashMap::new(),
                                None => {
                                    return Err(FragmentReplaceError::SplitTailAttributesRequired)
                                }
                            };
                        }
                        block.children = suffix;
                        replacement.push(block);
                    }
                    (
                        replace_blocks(parent_path, start_index, end_index, replacement),
                        selection,
                    )
                }
            }
        }
    };
    let result = apply(
        doc,
        Transaction::new(steps, options.actor_kind, options.actor_id),
    )?;
    Ok((result, selection))
}

fn block_at<'a>(doc: &'a BlockNode, path: &[usize]) -> Option<&'a BlockNode> {
    path.iter()
        .try_fold(doc, |node, &index| node.children.get(index)?.as_block())
}

fn endpoint<'a, 'p>(
    doc: &'a BlockNode,
    position: &'p DocPosition,
) -> Result<(&'p [usize], &'a BlockNode), FragmentReplaceError> {
    let invalid = || FragmentReplaceError::InvalidPosition(position.clone());
    let (&index, parent_path) = position.path.split_last().ok_or_else(invalid)?;
    let parent = block_at(doc, parent_path).ok_or_else(invalid)?;
    if !parent.kind.holds_inline_content() {
        return Err(invalid());
    }
    if parent.children.is_empty() {
        return if index == 0 && position.char_offset == 0 {
            Ok((parent_path, parent))
        } else {
            Err(invalid())
        };
    }
    let leaf = parent
        .children
        .get(index)
        .and_then(Child::as_text)
        .ok_or_else(invalid)?;
    if position.char_offset > leaf.text.len_chars() {
        return Err(invalid());
    }
    Ok((parent_path, parent))
}

fn before(block: &BlockNode, position: &DocPosition) -> Vec<Child> {
    if block.children.is_empty() {
        return Vec::new();
    }
    let index = position.path[position.path.len() - 1];
    let mut children = block.children[..index].to_vec();
    let leaf = block.children[index].as_text().expect("validated endpoint");
    if position.char_offset > 0 {
        children.push(Child::Text(TextLeaf::with_marks(
            &leaf.text.slice_chars(0, position.char_offset),
            leaf.marks.clone(),
        )));
    }
    children
}

fn after(block: &BlockNode, position: &DocPosition) -> Vec<Child> {
    if block.children.is_empty() {
        return Vec::new();
    }
    let index = position.path[position.path.len() - 1];
    let leaf = block.children[index].as_text().expect("validated endpoint");
    let mut children = Vec::new();
    if position.char_offset < leaf.text.len_chars() {
        children.push(Child::Text(TextLeaf::with_marks(
            &leaf
                .text
                .slice_chars(position.char_offset, leaf.text.len_chars()),
            leaf.marks.clone(),
        )));
    }
    children.extend_from_slice(&block.children[index + 1..]);
    children
}

fn validate_blocks(blocks: &[BlockNode]) -> Result<(), FragmentReplaceError> {
    if blocks.is_empty() {
        return Err(FragmentReplaceError::EmptyBlockFragment);
    }
    schema::validate_tree(&BlockNode::doc(blocks.to_vec())).map_err(TransformError::from)?;
    Ok(())
}

fn replace_blocks(parent: &[usize], start: usize, end: usize, blocks: Vec<BlockNode>) -> Vec<Step> {
    let mut steps: Vec<_> = (start..=end)
        .rev()
        .map(|index| Step::DeleteNode {
            parent_path: parent.to_vec(),
            index,
        })
        .collect();
    steps.extend(
        blocks
            .into_iter()
            .enumerate()
            .map(|(offset, node)| Step::InsertNode {
                parent_path: parent.to_vec(),
                index: start + offset,
                node,
            }),
    );
    steps
}

fn block_end_selection(blocks: &[BlockNode], parent: &[usize], start: usize) -> Selection {
    let mut path = parent.to_vec();
    path.push(start + blocks.len() - 1);
    match last_text_position(blocks.last().expect("nonempty validated fragment"), &path) {
        Some(position) => Selection::caret(position),
        None => Selection::node(path),
    }
}

fn last_text_position(block: &BlockNode, path: &[usize]) -> Option<DocPosition> {
    let (index, child) = block.children.iter().enumerate().next_back()?;
    let mut child_path = path.to_vec();
    child_path.push(index);
    match child {
        Child::Text(leaf) => Some(DocPosition::new(child_path, leaf.text.len_chars())),
        Child::Block(block) => last_text_position(block, &child_path),
        Child::HsLink(_) | Child::Transclusion(_) => None,
    }
}
