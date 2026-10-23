//! Document positions: a resolved path through the block tree plus a char offset
//! into a text leaf (WP-KERNEL-012 MT-011).
//!
//! [`DocPosition`] is the in-tree address the selection model and the transform
//! layer use: a `Vec<usize>` of child indices from the `doc` root down to a node,
//! plus a `char_offset` into the text leaf at the end of that path.
//!
//! [`resolve`] maps a flat absolute char offset (the unit a renderer or a
//! "caret at document char N" command uses) to a `DocPosition` by walking the tree
//! and accumulating each leaf's char length. [`absolute_offset`] is the inverse.
//! Both are panic-free: an out-of-range absolute offset clamps to the end of the
//! document.

use super::node::{BlockNode, Child};

/// A resolved position in the document tree.
///
/// `path` is the chain of child indices from the `doc` root to the containing
/// node. `char_offset` is the char index inside the text leaf at the end of the
/// path. For a position that lands on a block boundary (between leaves) the path
/// ends at the text leaf and `char_offset` is its length / 0 accordingly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocPosition {
    /// Child indices from the root down to (and including) the addressed text leaf.
    pub path: Vec<usize>,
    /// Char offset into the text leaf at the end of `path`.
    pub char_offset: usize,
}

impl DocPosition {
    /// Build a position from a path and char offset.
    pub fn new(path: Vec<usize>, char_offset: usize) -> Self {
        Self { path, char_offset }
    }
}

/// Resolve a flat absolute CHAR offset into a [`DocPosition`] by walking `doc` in
/// document order, accumulating each text leaf's char length until the running
/// total reaches `abs`. An offset past the end of the document clamps to the last
/// text leaf's end.
///
/// Returns `None` only when the document has NO text leaf at all (e.g. an empty
/// `doc` with no paragraph), because there is then no leaf to address.
pub fn resolve(doc: &BlockNode, abs: usize) -> Option<DocPosition> {
    let mut remaining = abs;
    let mut last_leaf: Option<DocPosition> = None;
    walk_resolve(
        doc,
        &mut Vec::new(),
        &mut remaining,
        &mut last_leaf,
        &mut None,
    )
    // If `found` short-circuited it is returned by walk_resolve; otherwise the
    // offset ran past the end and we clamp to the last seen leaf.
    .or(last_leaf)
}

/// Recursive helper for [`resolve`]. Returns `Some(pos)` as soon as the target leaf
/// is found; otherwise threads `last_leaf` (the most recent text leaf seen) so the
/// caller can clamp a past-the-end offset.
fn walk_resolve(
    node: &BlockNode,
    path: &mut Vec<usize>,
    remaining: &mut usize,
    last_leaf: &mut Option<DocPosition>,
    found: &mut Option<DocPosition>,
) -> Option<DocPosition> {
    for (i, child) in node.children.iter().enumerate() {
        path.push(i);
        match child {
            Child::Text(leaf) => {
                let len = leaf.text.len_chars();
                // The target lands in this leaf when remaining <= len (<= so a
                // caret at the very end of a leaf resolves to this leaf, not the next).
                if *remaining <= len {
                    *found = Some(DocPosition::new(path.clone(), *remaining));
                    path.pop();
                    return found.clone();
                }
                *remaining -= len;
                *last_leaf = Some(DocPosition::new(path.clone(), len));
            }
            Child::Block(b) => {
                if let Some(pos) = walk_resolve(b, path, remaining, last_leaf, found) {
                    path.pop();
                    return Some(pos);
                }
            }
            Child::HsLink(_) | Child::Transclusion(_) => {
                // An inline atom (hsLink or loomTransclusion) occupies one position
                // unit but hosts no caret INTERIOR; a flat offset that lands on it
                // resolves to the offset just past it (the next text leaf / boundary),
                // so we just consume its single unit and keep walking.
                if *remaining == 0 {
                    // A caret sitting just before this atom: clamp to the last text
                    // leaf boundary if one exists; otherwise fall through.
                    if let Some(pos) = last_leaf.clone() {
                        *found = Some(pos.clone());
                        path.pop();
                        return Some(pos);
                    }
                }
                *remaining = remaining.saturating_sub(1);
            }
        }
        path.pop();
    }
    None
}

/// The inverse of [`resolve`]: the flat absolute char offset of a [`DocPosition`].
/// Walks the tree counting char lengths of every text leaf that comes before the
/// addressed leaf in document order, then adds `pos.char_offset`. An invalid path
/// (out-of-range index) clamps by summing what it can reach and returning that.
pub fn absolute_offset(doc: &BlockNode, pos: &DocPosition) -> usize {
    let mut acc = 0usize;
    let mut node = doc;
    for (depth, &idx) in pos.path.iter().enumerate() {
        let is_last = depth + 1 == pos.path.len();
        // Sum the char length of every sibling BEFORE idx at this level.
        for sib in node.children.iter().take(idx) {
            acc += sib.char_len();
        }
        match node.children.get(idx) {
            Some(Child::Text(_)) if is_last => {
                // The addressed leaf: add the in-leaf offset and stop.
                return acc + pos.char_offset;
            }
            Some(Child::Block(b)) => node = b,
            // Path points at a text leaf but is not the last element, or index is
            // out of range: clamp to what we have accumulated.
            _ => return acc,
        }
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::super::node::{BlockNode, Child, NodeKind, TextLeaf};
    use super::*;

    fn sample_doc() -> BlockNode {
        // doc > [ paragraph("hello"), paragraph("world") ]
        BlockNode::doc(vec![
            BlockNode::paragraph("hello"),
            BlockNode::paragraph("world"),
        ])
    }

    #[test]
    fn resolve_within_first_leaf() {
        let doc = sample_doc();
        let pos = resolve(&doc, 3).unwrap();
        // path: doc.child[0] (paragraph) -> child[0] (text), offset 3 ("hel|lo")
        assert_eq!(pos.path, vec![0, 0]);
        assert_eq!(pos.char_offset, 3);
        assert_eq!(absolute_offset(&doc, &pos), 3);
    }

    #[test]
    fn resolve_crosses_into_second_leaf() {
        let doc = sample_doc();
        // "hello" is 5 chars; offset 7 = 2 into "world".
        let pos = resolve(&doc, 7).unwrap();
        assert_eq!(pos.path, vec![1, 0]);
        assert_eq!(pos.char_offset, 2);
        assert_eq!(absolute_offset(&doc, &pos), 7);
    }

    #[test]
    fn resolve_past_end_clamps_to_last_leaf() {
        let doc = sample_doc();
        let pos = resolve(&doc, 999).unwrap();
        assert_eq!(pos.path, vec![1, 0]);
        assert_eq!(pos.char_offset, 5); // end of "world"
    }

    #[test]
    fn round_trip_all_offsets() {
        let doc = sample_doc();
        // total text chars = 10; every offset 0..=10 round-trips.
        for abs in 0..=10 {
            let pos = resolve(&doc, abs).unwrap();
            assert_eq!(absolute_offset(&doc, &pos), abs, "offset {abs}");
        }
    }

    #[test]
    fn resolve_empty_doc_is_none() {
        let doc = BlockNode::new(NodeKind::Doc); // no children at all
        assert_eq!(resolve(&doc, 0), None);
    }

    #[test]
    fn resolve_empty_paragraph_addresses_the_leaf() {
        let mut p = BlockNode::new(NodeKind::Paragraph);
        p.children.push(Child::Text(TextLeaf::new("")));
        let doc = BlockNode::doc(vec![p]);
        let pos = resolve(&doc, 0).unwrap();
        assert_eq!(pos.path, vec![0, 0]);
        assert_eq!(pos.char_offset, 0);
    }
}
