//! Layout behind a port (decision `layout_and_text`: no Taffy; a port keeps another solver possible). [`StackLayout`]
//! is row and column stacking with token sizes: Fixed, Hug (measured content) and Fill (share of what is left).
use crate::geom::Rect;
use crate::text::Text;
use crate::tree::{Align, Axis, NodeId, Paint, Size, Tree};

/// A layout solver: sets `bounds` of `root` and every descendant inside `avail` (logical px).
pub trait LayoutPort {
    fn layout(&mut self, tree: &mut Tree, root: NodeId, avail: Rect, text: &mut Text);
}

/// Row/column stack layout.
#[derive(Clone, Copy, Debug, Default)]
pub struct StackLayout;

impl LayoutPort for StackLayout {
    fn layout(&mut self, tree: &mut Tree, root: NodeId, avail: Rect, text: &mut Text) {
        if tree.is_empty() {
            return;
        }
        let mut measured = vec![(0.0f32, 0.0f32); tree.len()];
        measure(tree, root, text, &mut measured, 0);
        place(tree, root, avail, &measured, 0);
    }
}

/// Deepest nesting the stack layout follows; deeper nodes keep empty bounds.
pub const MAX_DEPTH: usize = 256;

fn content(tree: &Tree, id: NodeId, text: &mut Text) -> (f32, f32) {
    match &tree.get(id).paint {
        Paint::Label { text: s, token, .. } => {
            let st = token.style();
            (text.measure(s, *token, 0).ceil(), st.line)
        }
        Paint::Value { text: s, unit } => {
            let st = crate::tokens::TextToken::Value.style();
            let mut w = text.measure(s, crate::tokens::TextToken::Value, 0);
            if let Some(u) = unit {
                w += 2.0 + text.measure(u, crate::tokens::TextToken::Caption, 0);
            }
            (w.ceil(), st.line)
        }
        Paint::Icon { size, .. } => (*size, *size),
        _ => (0.0, 0.0),
    }
}

fn measure(tree: &Tree, id: NodeId, text: &mut Text, out: &mut [(f32, f32)], depth: usize) -> (f32, f32) {
    let node = tree.get(id);
    let l = node.layout;
    let (mut cw, mut ch) = content(tree, id, text);
    if depth < MAX_DEPTH && !node.children.is_empty() {
        let (mut main, mut cross) = (0.0f32, 0.0f32);
        let n = node.children.len();
        for &c in &node.children {
            let (w, h) = measure(tree, c, text, out, depth + 1);
            let cl = tree.get(c).layout;
            let w = match cl.w {
                Size::Fixed(v) => v,
                Size::Hug => w,
                Size::Fill => w,
            };
            let h = match cl.h {
                Size::Fixed(v) => v,
                Size::Hug => h,
                Size::Fill => h,
            };
            let (m, x) = if l.axis == Axis::Row { (w, h) } else { (h, w) };
            main += m;
            cross = cross.max(x);
        }
        main += l.gap * (n.saturating_sub(1)) as f32;
        let (w, h) = if l.axis == Axis::Row { (main, cross) } else { (cross, main) };
        cw = cw.max(w);
        ch = ch.max(h);
    }
    let w = (cw + l.pad[0] + l.pad[2]).max(l.min_w);
    let h = ch + l.pad[1] + l.pad[3];
    let w = if let Size::Fixed(v) = l.w { v } else { w };
    let h = if let Size::Fixed(v) = l.h { v } else { h };
    out[id.0 as usize] = (w, h);
    (w, h)
}

fn place(tree: &mut Tree, id: NodeId, rect: Rect, m: &[(f32, f32)], depth: usize) {
    tree.get_mut(id).bounds = rect;
    if depth >= MAX_DEPTH || depth == 0 {
        // RED STUB: children are not placed.
        return;
    }
    let node = tree.get(id);
    let l = node.layout;
    let children = node.children.clone();
    if children.is_empty() {
        return;
    }
    let inner = rect.inset(l.pad[0], l.pad[1], l.pad[2], l.pad[3]);
    let row = l.axis == Axis::Row;
    let (avail_main, avail_cross) = if row { (inner.w(), inner.h()) } else { (inner.h(), inner.w()) };
    let mut sizes = Vec::with_capacity(children.len());
    let mut fixed = 0.0f32;
    let mut fills = 0usize;
    for &c in &children {
        let cl = tree.get(c).layout;
        let (mw, mh) = m[c.0 as usize];
        let (ms, mm) = if row { (cl.w, mw) } else { (cl.h, mh) };
        let s = match ms {
            Size::Fixed(v) => Some(v),
            Size::Hug => Some(mm),
            Size::Fill => None,
        };
        if let Some(v) = s {
            fixed += v;
        } else {
            fills += 1;
        }
        sizes.push(s);
    }
    let gaps = l.gap * (children.len().saturating_sub(1)) as f32;
    let left = (avail_main - fixed - gaps).max(0.0);
    let share = if fills > 0 { left / fills as f32 } else { 0.0 };
    let used = fixed + gaps + share * fills as f32;
    let mut pos = match (fills, l.main) {
        (0, Align::Center) => (avail_main - used) / 2.0,
        (0, Align::End) => avail_main - used,
        _ => 0.0,
    }
    .max(0.0);
    let (o_main, o_cross) = if row { (inner.x0, inner.y0) } else { (inner.y0, inner.x0) };
    for (i, &c) in children.iter().enumerate() {
        let cl = tree.get(c).layout;
        let (mw, mh) = m[c.0 as usize];
        let main = sizes[i].unwrap_or(share);
        let (cs, cm) = if row { (cl.h, mh) } else { (cl.w, mw) };
        let cross = match cs {
            Size::Fixed(v) => v,
            Size::Hug => cm.min(avail_cross.max(cm)),
            Size::Fill => avail_cross,
        };
        let off = match l.cross {
            Align::Start => 0.0,
            Align::Center => ((avail_cross - cross) / 2.0).round(),
            Align::End => avail_cross - cross,
        };
        let r = if row {
            Rect::xywh(o_main + pos, o_cross + off, main, cross)
        } else {
            Rect::xywh(o_cross + off, o_main + pos, cross, main)
        };
        place(tree, c, r, m, depth + 1);
        pos += main + l.gap;
    }
}
