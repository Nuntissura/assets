//! The passive scrollbar (components.json `scrollbar`): the lane is always reserved, the thumb and its node exist only
//! when the content overflows, the thumb keeps min_thumb 24, and its top tracks the first shown unit.
use kumihimo_ui::geom::{Rect, Scale};
use kumihimo_ui::text::{BoxText, Text};
use kumihimo_ui::tokens::Theme;
use kumihimo_ui::tree::{Align, Axis, Interaction, LayoutSpec, Node, Panel, Role, Size};
use kumihimo_ui::widgets;

fn build(lane_h: f32, first: usize, shown: usize, total: usize) -> Panel {
    let mut p = Panel::default();
    let ui = Interaction::default();
    let mut t = Text::new(Box::new(BoxText));
    p.update(1, &ui, Rect::xywh(0.0, 0.0, 40.0, lane_h), Scale::ONE, Theme::Dark, &mut t, |b| {
        b.begin(Node::new("studio.panel.test", Role::Group).layout(LayoutSpec { axis: Axis::Row, w: Size::Fill, h: Size::Fill, cross: Align::Start, ..LayoutSpec::default() }));
        b.leaf(Node::deco().size(Size::Fill, Size::Fill));
        widgets::scrollbar(b, "studio.scroll.test", "Test", lane_h, first, shown, total);
        b.end();
    })
    .unwrap();
    p
}

#[test]
fn scrollbar_absent_when_content_fits_and_tracks_first_row() {
    let fits = build(300.0, 0, 10, 5);
    assert!(fits.tree().find("studio.scroll.test").is_none(), "absent when content fits");
    // The lane stays reserved: the filler ends 10 px before the panel edge.
    assert_eq!(fits.tree().nodes.iter().filter(|n| n.bounds.w() == 10.0).count(), 1);

    let top = build(300.0, 0, 10, 100);
    let id = top.tree().find("studio.scroll.test").expect("overflow shows the scrollbar");
    let node = top.tree().get(id);
    assert_eq!((node.bounds.x0, node.bounds.w(), node.bounds.h()), (30.0, 10.0, 300.0));
    assert_eq!(node.value.as_deref(), Some("1 to 10 of 100"));
    assert!(node.actions.is_empty(), "passive");
    let thumb = top.tree().get(node.children[1]);
    assert_eq!((thumb.bounds.w(), thumb.bounds.h(), thumb.bounds.y0, thumb.bounds.x0), (6.0, 30.0, 0.0, 32.0));

    let bottom = build(300.0, 90, 10, 100);
    let n = bottom.tree().get(bottom.tree().find("studio.scroll.test").unwrap());
    assert_eq!(bottom.tree().get(n.children[1]).bounds.y1, 300.0, "last page: thumb at the lane end");
    // A first past the range is clamped, never drawn outside the lane.
    let past = build(300.0, 500, 10, 100);
    let n = past.tree().get(past.tree().find("studio.scroll.test").unwrap());
    assert_eq!(past.tree().get(n.children[1]).bounds.y1, 300.0);

    let long = build(300.0, 0, 10, 100_000);
    let n = long.tree().get(long.tree().find("studio.scroll.test").unwrap());
    assert_eq!(long.tree().get(n.children[1]).bounds.h(), 24.0, "min_thumb");
}
