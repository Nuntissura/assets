//! Selection model for the rich-text editor (WP-KERNEL-012 MT-011).
//!
//! Two selection shapes, mirroring ProseMirror:
//! - [`Selection::Text`] — an anchor/head pair of [`DocPosition`]s describing a
//!   text range (a collapsed range = a caret when `anchor == head`).
//! - [`Selection::Node`] — a whole-node selection addressed by its tree path
//!   (`node_path`), used when a block atom (image embed, horizontal rule) is
//!   selected as a unit.
//!
//! The renderer (MT-012) and the command layer (MT-013+) consume this; the model
//! MT only needs the typed shape so transactions can carry a selection alongside
//! their steps later. No UI here.

use super::position::DocPosition;

/// The editor's current selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// A text range from `anchor` to `head`. When `anchor == head` the selection is
    /// a collapsed caret. `anchor` is the fixed end (where the selection started);
    /// `head` is the moving end (where the caret is) — the same anchor/head
    /// convention the code editor's `CursorSet` uses.
    Text {
        anchor: DocPosition,
        head: DocPosition,
    },
    /// A whole-node selection addressed by its child-index path from the doc root.
    Node { node_path: Vec<usize> },
}

impl Selection {
    /// A collapsed caret at `pos`.
    pub fn caret(pos: DocPosition) -> Self {
        Selection::Text {
            anchor: pos.clone(),
            head: pos,
        }
    }

    /// A text range from `anchor` to `head`.
    pub fn text(anchor: DocPosition, head: DocPosition) -> Self {
        Selection::Text { anchor, head }
    }

    /// A whole-node selection.
    pub fn node(node_path: Vec<usize>) -> Self {
        Selection::Node { node_path }
    }

    /// True when this is a collapsed text caret (anchor == head). A node selection
    /// is never collapsed.
    pub fn is_collapsed(&self) -> bool {
        match self {
            Selection::Text { anchor, head } => anchor == head,
            Selection::Node { .. } => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_is_collapsed() {
        let p = DocPosition::new(vec![0, 0], 3);
        assert!(Selection::caret(p).is_collapsed());
    }

    #[test]
    fn range_is_not_collapsed() {
        let a = DocPosition::new(vec![0, 0], 1);
        let h = DocPosition::new(vec![0, 0], 4);
        assert!(!Selection::text(a, h).is_collapsed());
    }

    #[test]
    fn node_selection_is_never_collapsed() {
        assert!(!Selection::node(vec![2]).is_collapsed());
    }
}
