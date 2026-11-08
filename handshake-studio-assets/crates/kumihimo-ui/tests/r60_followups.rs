//! Lane SMB-R60-FOLLOWUPS-1 items 7 and 8 (validation round R60 attempt 2 probes u1 and u2): AC-GEOM-03's
//! check compares every adjacent pair's snapped edges, and a Hug child never takes more cross room than its
//! parent has, so AC-TEXT-01 sees a label wider than its column.
use kumihimo_ui::checks::{clipped_labels, edge_gaps};
use kumihimo_ui::geom::{Rect, Scale};
use kumihimo_ui::layout::{LayoutPort, StackLayout};
use kumihimo_ui::text::{BoxText, Text};
use kumihimo_ui::tokens::TextToken;
use kumihimo_ui::tree::{Align, Axis, Builder, Interaction, LayoutSpec, Node, Paint, Role, Size};

fn text() -> Text {
    Text::new(Box::new(BoxText))
}

/// Item 7 (probe u1): two docked panes a logical pixel apart are not one shared edge at any scale, so the
/// check names the pair with the physical gap; panes that share the logical edge stay silent.
#[test]
fn r60_u1_edge_gaps_reports_adjacent_panes_that_do_not_share_an_edge() {
    let apart = [Rect::new(0.0, 0.0, 100.0, 50.0), Rect::new(101.0, 0.0, 200.0, 50.0)];
    let shared = [Rect::new(0.0, 0.0, 100.0, 50.0), Rect::new(100.0, 0.0, 200.0, 50.0)];
    let overlap = [Rect::new(0.0, 0.0, 100.0, 50.0), Rect::new(99.0, 0.0, 200.0, 50.0)];
    for (s, gap) in [(1.0f32, 1), (1.25, 1), (2.0, 2)] {
        let scale = Scale::new(s).unwrap();
        assert_eq!(edge_gaps(&apart, scale), vec![(0, gap)], "a 1 logical px gap at scale {s}");
        assert!(edge_gaps(&shared, scale).is_empty(), "a shared edge at scale {s}");
        assert_eq!(edge_gaps(&overlap, scale).len(), 1, "an overlap is no shared edge either, scale {s}");
    }
}

/// Item 8 (probe u2): a column 40 px wide holding a Hug label far wider than 40 px. The label's box stays
/// inside the column (the cross size is clamped to the room there is), and AC-TEXT-01 reports the clip.
#[test]
fn r60_u2_hug_label_wider_than_its_column_is_kept_inside_and_reported_clipped() {
    let ui = Interaction::default();
    let column = LayoutSpec { axis: Axis::Column, w: Size::Fixed(40.0), h: Size::Fixed(200.0), cross: Align::Start, ..LayoutSpec::default() };
    let mut b = Builder::new(Node::new("studio.test.column", Role::Group).layout(column), &ui);
    let label = "A label far wider than its forty pixel column";
    b.leaf(Node::deco().paint(Paint::Label { text: label.into(), token: TextToken::Body, weight: 0, color: None, align: Align::Start, ellipsis: false }));
    let mut t = b.finish().unwrap();
    let root = t.root();
    let mut tx = text();
    StackLayout.layout(&mut t, root, Rect::xywh(0.0, 0.0, 40.0, 200.0), &mut tx);
    let wide = tx.measure(label, TextToken::Body, 0);
    assert!(wide > 100.0, "the fixture label is wide: {wide}");
    let col = t.get(root).bounds;
    let lab = t.get(t.get(root).children[0]).bounds;
    assert!(lab.x0 >= col.x0 && lab.x1 <= col.x1, "the Hug label stays inside its column: label {lab:?}, column {col:?}");
    let clipped = clipped_labels(&t, &mut tx);
    assert_eq!(clipped.len(), 1, "{clipped:?}");
    assert_eq!(clipped[0].owner, "studio.test.column");
    assert!((clipped[0].overflow_px - (wide - 40.0)).abs() < 1.0, "the overflow is what does not fit: {clipped:?}");

    // Centred children are clamped the same way (the probe's case): no negative offset, no box outside.
    let centred = LayoutSpec { cross: Align::Center, ..column };
    let mut b = Builder::new(Node::new("studio.test.centred", Role::Group).layout(centred), &ui);
    b.leaf(Node::deco().paint(Paint::Label { text: label.into(), token: TextToken::Body, weight: 0, color: None, align: Align::Start, ellipsis: false }));
    let mut t = b.finish().unwrap();
    let root = t.root();
    StackLayout.layout(&mut t, root, Rect::xywh(0.0, 0.0, 40.0, 200.0), &mut tx);
    let lab = t.get(t.get(root).children[0]).bounds;
    assert!(lab.x0 >= 0.0 && lab.x1 <= 40.0, "centred Hug label inside: {lab:?}");
    assert_eq!(clipped_labels(&t, &mut tx).len(), 1);

    // A Hug child that fits keeps its measured size.
    let mut b = Builder::new(Node::new("studio.test.fits", Role::Group).layout(LayoutSpec { w: Size::Fixed(400.0), ..column }), &ui);
    b.leaf(Node::deco().paint(Paint::Label { text: label.into(), token: TextToken::Body, weight: 0, color: None, align: Align::Start, ellipsis: false }));
    let mut t = b.finish().unwrap();
    let root = t.root();
    StackLayout.layout(&mut t, root, Rect::xywh(0.0, 0.0, 400.0, 200.0), &mut tx);
    let lab = t.get(t.get(root).children[0]).bounds;
    assert_eq!(lab.w(), wide.ceil(), "a fitting Hug label keeps its measured width");
    assert!(clipped_labels(&t, &mut tx).is_empty());
}
