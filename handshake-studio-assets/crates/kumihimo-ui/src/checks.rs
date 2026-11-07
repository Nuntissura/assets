//! Tree-method acceptance checks of gui-design/acceptance-checks.json that need no screenshot, so tests, `gui.tree`
//! and validators run the same code: AC-GEOM-02 (no overlap between interactive controls), AC-GEOM-03 (shared physical
//! edges), AC-TEXT-01 (no clipped label), AC-TEXT-03 (text floor).
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
        if w > avail + 0.01 {
            out.push(Clipped { owner: owner_of(tree, id), text: s.to_owned(), overflow_px: w - avail });
        }
    }
    out
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
