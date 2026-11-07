//! Hit test and focus order over a laid-out tree. Hit test returns the topmost interactive node under a point (later
//! siblings draw over earlier ones), never through a clipping ancestor's outside. Focus order is reading order of
//! focusable, enabled nodes (components.json keyboard: Tab and Shift+Tab move in reading order).
use crate::geom::Point;
use crate::tree::{NodeId, Tree};

pub fn hit_test(tree: &Tree, p: Point) -> Option<NodeId> {
    if tree.is_empty() {
        return None;
    }
    hit_in(tree, NodeId(0), p, 0)
}

fn hit_in(tree: &Tree, id: NodeId, p: Point, depth: usize) -> Option<NodeId> {
    let n = tree.get(id);
    let inside = n.bounds.contains(p);
    if n.flags.clip && !inside {
        return None;
    }
    if depth < crate::layout::MAX_DEPTH {
        for &c in n.children.iter().rev() {
            if let Some(h) = hit_in(tree, c, p, depth + 1) {
                return Some(h);
            }
        }
    }
    (inside && n.interactive()).then_some(id)
}

/// Focusable, enabled, visible nodes in reading order. A node with empty bounds, one clipped out by an ancestor, or one
/// deeper than layout follows is not a stop (F8): the ring would be drawn where nobody sees it.
pub fn focus_order(tree: &Tree) -> Vec<NodeId> {
    tree.preorder()
        .into_iter()
        .filter(|&id| {
            let n = tree.get(id);
            n.flags.focusable && !n.author_id.is_empty() && !tree.effectively_disabled(id) && visible(tree, id)
        })
        .collect()
}

/// True when some of `id`'s bounds survive every clipping ancestor and it lies within layout depth.
pub fn visible(tree: &Tree, id: NodeId) -> bool {
    let mut r = tree.get(id).bounds;
    let mut at = id;
    let mut depth = 0usize;
    while let Some(p) = tree.get(at).parent {
        depth += 1;
        if tree.get(p).flags.clip {
            r = r.intersect(&tree.get(p).bounds);
        }
        at = p;
    }
    depth < crate::layout::MAX_DEPTH && !r.is_empty()
}

/// The next (or previous) focus stop after `current`, wrapping; the first (last) stop when nothing has focus.
pub fn focus_step(tree: &Tree, current: Option<&str>, back: bool) -> Option<NodeId> {
    let order = focus_order(tree);
    if order.is_empty() {
        return None;
    }
    let at = current.and_then(|c| order.iter().position(|&id| tree.get(id).author_id == c));
    let n = order.len();
    let i = match (at, back) {
        (None, false) => 0,
        (None, true) => n - 1,
        (Some(i), false) => (i + 1) % n,
        (Some(i), true) => (i + n - 1) % n,
    };
    order.get(i).copied()
}
