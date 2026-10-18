//! Compile-time document schema: which node kinds may contain which children, and
//! which marks a text leaf may carry (WP-KERNEL-012 MT-011).
//!
//! The schema is a `const fn` / `match`-based table (MT impl note 5: a compile-time
//! const table, not a runtime-loaded file). [`validate_node`] is run on EVERY
//! transaction apply ([`super::transform::apply_transaction`]); a violation makes
//! the whole transaction roll back and return `Err`, so the document can never enter
//! a structurally-corrupt intermediate state (red-team RISK-2).
//!
//! The content model mirrors ProseMirror's: inline-content containers
//! (`paragraph`, `heading`, `code_block`) hold text leaves; structural containers
//! hold block children; atoms hold nothing. The allowed-mark axis answers
//! "may this run carry this mark" — every mark is allowed on prose runs, but a
//! `code_block`'s text run carries NO marks (matching the Tiptap code-block schema,
//! where `marks: ""`).

use thiserror::Error;

use super::node::{BlockNode, Child, Mark, NodeKind};

/// Why a node failed schema validation. Returned (never panicked) so a bad
/// transaction rolls the document back cleanly.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SchemaError {
    /// A block child appeared under a kind that only holds inline text (e.g. a
    /// paragraph nested directly inside another paragraph's inline content).
    #[error("node {parent:?} may not contain a block child {child:?}")]
    BlockChildNotAllowed { parent: NodeKind, child: NodeKind },
    /// A text leaf appeared under a kind that only holds block children (e.g. raw
    /// text directly inside a `doc` or a `bullet_list`).
    #[error("node {parent:?} may not contain inline text directly")]
    TextChildNotAllowed { parent: NodeKind },
    /// An inline atom (`hsLink`) appeared under a kind that does not allow inline
    /// atoms (only `paragraph`/`heading` do; a `code_block` holds plain text only).
    #[error("node {parent:?} may not contain an inline hsLink atom")]
    InlineAtomNotAllowed { parent: NodeKind },
    /// An atom kind (`hard_break`, `horizontal_rule`) was given children.
    #[error("atom node {parent:?} may not contain children")]
    AtomHasChildren { parent: NodeKind },
    /// A mark was applied to a run inside a node that forbids it (e.g. any mark on a
    /// `code_block`'s text).
    #[error("node {parent:?} forbids mark {mark_type} on its text")]
    MarkNotAllowed {
        parent: NodeKind,
        mark_type: &'static str,
    },
    /// More than one `doc` root, or a `doc` nested below the root.
    #[error("doc node may only appear as the single tree root")]
    MisplacedDoc,
}

/// True when `parent` may directly contain a block child of `child` kind. A `doc`
/// holds top-level blocks; lists hold list/task items; list items, blockquotes,
/// table structures hold their structural children; inline containers + atoms hold
/// no block children.
///
/// A nested `doc` is always rejected here (a doc may only be the root —
/// [`validate_tree`] enforces the single-root rule).
pub fn block_child_allowed(parent: NodeKind, child: NodeKind) -> bool {
    if matches!(child, NodeKind::Doc) {
        return false; // a doc is never a child of anything.
    }
    match parent {
        // The root holds any top-level block except another doc.
        NodeKind::Doc => true,
        // A list holds list items / task items.
        NodeKind::OrderedList | NodeKind::BulletList => {
            matches!(child, NodeKind::ListItem | NodeKind::TaskItem)
        }
        // A list item / task item / blockquote / table (header|body) cell holds block content
        // (paragraphs, nested lists, etc.) — any non-doc block. A TableHeader cell has the SAME
        // content model as a TableCell (MT-020 amendment).
        NodeKind::ListItem
        | NodeKind::TaskItem
        | NodeKind::Blockquote
        | NodeKind::TableCell
        | NodeKind::TableHeader => true,
        // Tables hold rows; rows hold body cells AND header cells (a header ROW is a tableRow whose
        // cells are tableHeader nodes — the real Tiptap shape, MT-020 amendment).
        NodeKind::Table => matches!(child, NodeKind::TableRow),
        NodeKind::TableRow => matches!(child, NodeKind::TableCell | NodeKind::TableHeader),
        // Inline-content containers hold NO block children.
        NodeKind::Paragraph | NodeKind::Heading(_) | NodeKind::CodeBlock => false,
        // Atoms hold nothing.
        NodeKind::HardBreak | NodeKind::HorizontalRule => false,
    }
}

/// True when `parent` may directly contain inline text leaves. Only the three
/// inline-content containers do.
pub fn text_child_allowed(parent: NodeKind) -> bool {
    parent.holds_inline_content()
}

/// True when `parent` may directly contain an inline atom (`hsLink`). Only
/// `paragraph` and `heading` hold inline atoms; a `code_block` is plain-text only
/// (the Tiptap code block has no inline node content), so an `hsLink` there is
/// rejected.
pub fn inline_atom_allowed(parent: NodeKind) -> bool {
    matches!(parent, NodeKind::Paragraph | NodeKind::Heading(_))
}

/// True when a text run inside `parent` may carry `mark`. Every prose run allows
/// every mark; a `code_block`'s run carries NO marks (Tiptap `code_block` has
/// `marks: ""`). A heading allows the full mark set (matching StarterKit).
pub fn mark_allowed(parent: NodeKind, _mark: &Mark) -> bool {
    !matches!(parent, NodeKind::CodeBlock)
}

/// Validate a single node's DIRECT content model: its children are the right kind
/// (block vs text) for its kind, atoms have no children, and its text runs carry
/// only allowed marks. Does NOT recurse — [`validate_tree`] walks the whole tree.
/// This is the per-node check `apply_transaction` runs on the touched node.
pub fn validate_node(node: &BlockNode) -> Result<(), SchemaError> {
    if node.kind.is_atom() {
        if !node.children.is_empty() {
            return Err(SchemaError::AtomHasChildren { parent: node.kind });
        }
        return Ok(());
    }
    for child in &node.children {
        match child {
            Child::Block(b) => {
                if !block_child_allowed(node.kind, b.kind) {
                    return Err(SchemaError::BlockChildNotAllowed {
                        parent: node.kind,
                        child: b.kind,
                    });
                }
            }
            Child::Text(leaf) => {
                if !text_child_allowed(node.kind) {
                    return Err(SchemaError::TextChildNotAllowed { parent: node.kind });
                }
                for mark in &leaf.marks {
                    if !mark_allowed(node.kind, mark) {
                        return Err(SchemaError::MarkNotAllowed {
                            parent: node.kind,
                            mark_type: mark.json_type(),
                        });
                    }
                }
            }
            Child::HsLink(_) | Child::Transclusion(_) => {
                // Both inline atoms (hsLink wikilink, loomTransclusion reference) live in the same
                // inline-content slot as text runs (paragraph/heading), enforced identically.
                if !inline_atom_allowed(node.kind) {
                    return Err(SchemaError::InlineAtomNotAllowed { parent: node.kind });
                }
            }
        }
    }
    Ok(())
}

/// Recursively validate the whole tree rooted at `root`. Enforces:
/// - `root` is a `Doc` and no descendant is a `Doc` (single-root rule),
/// - every node passes [`validate_node`].
///
/// Called by `apply_transaction` after every step batch so a transaction that would
/// leave ANY node invalid is rolled back atomically.
pub fn validate_tree(root: &BlockNode) -> Result<(), SchemaError> {
    if !matches!(root.kind, NodeKind::Doc) {
        return Err(SchemaError::MisplacedDoc);
    }
    validate_subtree(root, true)
}

/// Recurse through a subtree. `is_root` is true only for the top `doc`; a `doc`
/// found anywhere else is a `MisplacedDoc`.
fn validate_subtree(node: &BlockNode, is_root: bool) -> Result<(), SchemaError> {
    if matches!(node.kind, NodeKind::Doc) && !is_root {
        return Err(SchemaError::MisplacedDoc);
    }
    validate_node(node)?;
    for child in &node.children {
        if let Child::Block(b) = child {
            validate_subtree(b, false)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::node::{HeadingLevel, TextLeaf};
    use super::*;

    #[test]
    fn paragraph_holds_text_not_blocks() {
        assert!(text_child_allowed(NodeKind::Paragraph));
        assert!(!block_child_allowed(
            NodeKind::Paragraph,
            NodeKind::Paragraph
        ));
    }

    #[test]
    fn doc_holds_blocks_not_text() {
        assert!(!text_child_allowed(NodeKind::Doc));
        assert!(block_child_allowed(NodeKind::Doc, NodeKind::Paragraph));
        assert!(!block_child_allowed(NodeKind::Doc, NodeKind::Doc));
    }

    #[test]
    fn code_block_forbids_marks() {
        assert!(!mark_allowed(NodeKind::CodeBlock, &Mark::Bold));
        assert!(mark_allowed(NodeKind::Paragraph, &Mark::Bold));
        assert!(mark_allowed(
            NodeKind::Heading(HeadingLevel::new(1)),
            &Mark::Bold
        ));
    }

    #[test]
    fn validate_node_rejects_text_in_doc() {
        let mut doc = BlockNode::new(NodeKind::Doc);
        doc.children.push(Child::Text(TextLeaf::new("oops")));
        assert_eq!(
            validate_node(&doc),
            Err(SchemaError::TextChildNotAllowed {
                parent: NodeKind::Doc
            })
        );
    }

    #[test]
    fn validate_node_rejects_atom_with_children() {
        let mut hr = BlockNode::new(NodeKind::HorizontalRule);
        hr.children.push(Child::Text(TextLeaf::new("x")));
        assert!(matches!(
            validate_node(&hr),
            Err(SchemaError::AtomHasChildren { .. })
        ));
    }
}
