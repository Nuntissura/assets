//! Tree-method acceptance checks of gui-design/acceptance-checks.json that need no screenshot, so tests, `gui.tree`
//! and validators run the same code: AC-GEOM-02 (no overlap between interactive controls), AC-GEOM-03 (shared physical
//! edges), AC-TEXT-01 (no clipped label), AC-TEXT-03 (text floor), AC-GEOM-01 (the frame's regions partition the
//! window).
use crate::geom::{Rect, Scale};
use crate::text::Text;
use crate::tree::{NodeId, Paint, Tree};

/// Pairs of interactive nodes (neither an ancestor of the other) whose bounds intersect: AC-GEOM-02.
pub fn overlapping_controls(tree: &Tree) -> Vec<(String, String)> {
    let ids: Vec<NodeId> = tree.preorder().into_iter().filter(|&i| tree.get(i).interactive()).collect();
    let mut out = Vec::new();
    for (k, &a) in ids.iter().enumerate() {
        for &b in &ids[k + 1..] {
            if tree.is_ancestor(a, b) || tree.is_ancestor(b, a) {
                continue;
            }
            if tree.get(a).bounds.intersects(&tree.get(b).bounds) {
                out.push((tree.get(a).author_id.clone(), tree.get(b).author_id.clone()));
            }
        }
    }
    out
}

/// A label whose shaped width exceeds its box where no ellipsis is declared: AC-TEXT-01.
#[derive(Clone, Debug, PartialEq)]
pub struct Clipped {
    /// author_id of the nearest identified ancestor.
    pub owner: String,
    pub text: String,
    pub overflow_px: f32,
}

pub fn clipped_labels(tree: &Tree, text: &mut Text) -> Vec<Clipped> {
    let mut out = Vec::new();
    for id in tree.preorder() {
        let n = tree.get(id);
        let (s, token, weight, ellipsis) = match &n.paint {
            Paint::Label { text: s, token, weight, ellipsis, .. } => (s.as_str(), *token, *weight, *ellipsis),
            Paint::Value { text: s, .. } => (s.as_str(), crate::tokens::TextToken::Value, 0, false),
            _ => continue,
        };
        if ellipsis {
            continue;
        }
        // Same weight and the same room emit uses: a value shares its field with its unit (F2, F3).
        let w = text.measure(s, token, weight);
        let avail = match &n.paint {
            Paint::Value { unit, .. } => {
                let unit_w = unit.as_deref().map_or(0.0, |u| text.measure(u, crate::tokens::TextToken::Caption, 0).ceil() + crate::tokens::spacing::S1);
                n.bounds.w() - 2.0 * crate::tokens::spacing::S3 - unit_w
            }
            _ => n.bounds.w(),
        };
        // A label is clipped when it is wider than its box, or when its box runs past an ancestor's (a Hug label in a
        // bar too narrow for it: the parent's clip or the window cuts it, SMB-GUI-FRAME-1).
        let past = ancestors_overflow(tree, id);
        let over = (w - avail).max(past);
        if over > 0.01 {
            out.push(Clipped { owner: owner_of(tree, id), text: s.to_owned(), overflow_px: over });
        }
    }
    out
}

/// How far `id`'s box runs past the right edge of its widest-reaching ancestor (0 when inside all of them).
fn ancestors_overflow(tree: &Tree, id: NodeId) -> f32 {
    let b = tree.get(id).bounds;
    let mut at = id;
    let mut over = 0.0f32;
    while let Some(p) = tree.get(at).parent {
        over = over.max(b.x1 - tree.get(p).bounds.x1);
        at = p;
    }
    over
}

fn owner_of(tree: &Tree, mut id: NodeId) -> String {
    loop {
        let n = tree.get(id);
        if !n.author_id.is_empty() {
            return n.author_id.clone();
        }
        match n.parent {
            Some(p) => id = p,
            None => return String::new(),
        }
    }
}

/// Labels using a style under 11 logical px: AC-TEXT-03 (styles come only from tokens, so this is the floor check).
pub fn under_text_floor(tree: &Tree) -> Vec<String> {
    tree.preorder()
        .into_iter()
        .filter_map(|id| match &tree.get(id).paint {
            Paint::Label { text, token, .. } if token.style().size < 11.0 => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// For rects that share a logical edge, the physical gap after snapping at `scale` (0 means shared): AC-GEOM-03.
/// Returns (index of leading rect, gap px) for every horizontally adjacent pair with a non-zero gap.
pub fn edge_gaps(rects: &[Rect], scale: Scale) -> Vec<(usize, i32)> {
    let mut out = Vec::new();
    for (i, w) in rects.windows(2).enumerate() {
        if w[0].x1 == w[1].x0 {
            let gap = scale.snap(w[1]).x0 - scale.snap(w[0]).x1;
            if gap != 0 {
                out.push((i, gap));
            }
        }
    }
    out
}

/// Why a set of region rects fails to partition a window (AC-GEOM-01), in physical px after [`Scale::snap`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PartitionFault {
    /// Rect `i` reaches outside the window.
    Outside(usize),
    /// Rects `i` and `j` share interior pixels.
    Overlap(usize, usize),
    /// Window pixels no rect covers.
    Uncovered(i64),
}

/// AC-GEOM-01: the rects, snapped at `scale`, cover `window` exactly and are pairwise disjoint (empty rects are allowed
/// members; an empty edge is passed as its 6 px seam rect, so it is a member, not a gap). Empty result = a partition.
pub fn partition(window: Rect, rects: &[Rect], scale: Scale) -> Vec<PartitionFault> {
    let w = scale.snap(window);
    let area = |r: &crate::geom::IRect| i64::from(r.w()) * i64::from(r.h());
    let snapped: Vec<crate::geom::IRect> = rects.iter().map(|r| scale.snap(*r)).collect();
    let mut out = Vec::new();
    for (i, r) in snapped.iter().enumerate() {
        if !r.is_empty() && (r.x0 < w.x0 || r.y0 < w.y0 || r.x1 > w.x1 || r.y1 > w.y1) {
            out.push(PartitionFault::Outside(i));
        }
    }
    let mut overlap = false;
    for (i, a) in snapped.iter().enumerate() {
        for (j, b) in snapped.iter().enumerate().skip(i + 1) {
            if !a.intersect(b).is_empty() {
                out.push(PartitionFault::Overlap(i, j));
                overlap = true;
            }
        }
    }
    if !overlap {
        let covered: i64 = snapped.iter().map(|r| area(&r.intersect(&w))).sum();
        let missing = area(&w) - covered;
        if missing != 0 {
            out.push(PartitionFault::Uncovered(missing));
        }
    }
    out
}
