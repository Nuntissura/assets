//! Hand-computed auto-layout oracles (points). Ids are `SLYR-0198a000-0000-7000-8000-<n hex>`;
//! numbers below are the `n`. Every expected rectangle is parent-local: (x, y, w, h).
use hsk_studio_accord::CancellationToken;
use hsk_studio_components::{
    AlignContent, AutoLayout, AxisSizing, ChildAlign, ChildSizing, ComponentsError as E,
    ConstraintKind, Constraints, CounterAlign, LayoutLimits, LayoutMode, LayoutNode, Placement,
    Positioning, PrimaryAlign, RectPt, SizeClamps, SizePt, WrapMode, solve_auto_layout,
};

fn bounded_case(name: &'static str, body: impl FnOnce() + Send + 'static) {
    use std::{
        panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
        sync::mpsc,
        time::Duration,
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = sender.send(catch_unwind(AssertUnwindSafe(body)));
    });
    match receiver.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(())) => {}
        Ok(Err(p)) => resume_unwind(p),
        Err(_) => panic!("case {name} exceeded declared 30-second deadline"),
    }
}

fn id(n: u32) -> String {
    format!("SLYR-0198a000-0000-7000-8000-{n:012x}")
}
fn leaf(n: u32, w: f64, h: f64) -> LayoutNode {
    LayoutNode::new(&id(n), w, h)
}
/// Gives `node` the auto layout and the sizing modes that agree with it (AUTO = hug, FIXED = fixed).
fn container(mut node: LayoutNode, layout: AutoLayout, children: Vec<LayoutNode>) -> LayoutNode {
    let hug = |a: AxisSizing| if a == AxisSizing::Auto { ChildSizing::Hug } else { ChildSizing::Fixed };
    let (p, c) = (hug(layout.primary_axis_sizing), hug(layout.counter_axis_sizing));
    let horizontal = layout.mode == LayoutMode::Horizontal;
    (node.sizing_horizontal, node.sizing_vertical) = if horizontal { (p, c) } else { (c, p) };
    node.layout = Some(layout);
    node.children = children;
    node
}
fn aligned(mut n: LayoutNode, a: ChildAlign) -> LayoutNode {
    n.align = a;
    n
}
/// FILL on the flow (primary) axis of a horizontal parent: `layout_grow = 1`.
fn grow_h(mut n: LayoutNode) -> LayoutNode {
    n.sizing_horizontal = ChildSizing::Fill;
    n.grow = 1.0;
    n
}
/// FILL on the cross axis of a horizontal parent: STRETCH.
fn stretch_v(mut n: LayoutNode) -> LayoutNode {
    n.sizing_vertical = ChildSizing::Fill;
    n.align = ChildAlign::Stretch;
    n
}
fn rect(x: f64, y: f64, w: f64, h: f64) -> RectPt {
    RectPt { x, y, w, h }
}
fn solve(root: &LayoutNode, size: Option<SizePt>) -> Result<hsk_studio_components::AutoLayoutResult, E> {
    solve_auto_layout(root, size, &LayoutLimits::default(), &CancellationToken::default())
}
fn boxes(r: &hsk_studio_components::AutoLayoutResult, ns: &[u32]) -> Vec<RectPt> {
    ns.iter().map(|n| r.get(&id(*n)).unwrap().rect).collect()
}

#[test]
fn flow_hug_gap_padding_alignment() {
    bounded_case("flow_hug_gap_padding_alignment", || {
        // Row, hug both axes, padding 10, gap 10. A 20x30 (min), B 40x10 (centre), C 30x50 (max).
        let tree = |mode: LayoutMode, gap: f64| {
            let mut l = AutoLayout::new(mode).with_padding(10.0);
            l.item_spacing = gap;
            container(
                leaf(1, 0.0, 0.0),
                l,
                vec![
                    leaf(2, 20.0, 30.0),
                    aligned(leaf(3, 40.0, 10.0), ChildAlign::Center),
                    aligned(leaf(4, 30.0, 50.0), ChildAlign::Max),
                ],
            )
        };
        // Width 20+40+30 + 2*10 gaps + 20 padding = 130; height max(30,10,50) + 20 = 70; inner
        // cross 50 => B y = 10 + (50-10)/2 = 30, C y = 10 + (50-50) = 10.
        let r = solve(&tree(LayoutMode::Horizontal, 10.0), None).unwrap();
        assert_eq!(
            boxes(&r, &[1, 2, 3, 4]),
            vec![
                rect(0.0, 0.0, 130.0, 70.0),
                rect(10.0, 10.0, 20.0, 30.0),
                rect(40.0, 30.0, 40.0, 10.0),
                rect(90.0, 10.0, 30.0, 50.0)
            ]
        );
        // Negative gap (-5) overlaps: width 90 - 10 + 20 = 100; B x = 10+20-5, C x = 25+40-5.
        let r = solve(&tree(LayoutMode::Horizontal, -5.0), None).unwrap();
        assert_eq!(
            boxes(&r, &[1, 2, 3, 4]),
            vec![
                rect(0.0, 0.0, 100.0, 70.0),
                rect(10.0, 10.0, 20.0, 30.0),
                rect(25.0, 30.0, 40.0, 10.0),
                rect(60.0, 10.0, 30.0, 50.0)
            ]
        );
        // Column: height 30+10+50 + 2*10 + 20 = 130; width max(20,40,30) + 20 = 60; inner cross
        // 40 => B x = 10, C x = 10 + (40-30) = 20; y = 10, 10+30+10, 50+10+10.
        let r = solve(&tree(LayoutMode::Vertical, 10.0), None).unwrap();
        assert_eq!(
            boxes(&r, &[1, 2, 3, 4]),
            vec![
                rect(0.0, 0.0, 60.0, 130.0),
                rect(10.0, 10.0, 20.0, 30.0),
                rect(10.0, 50.0, 40.0, 10.0),
                rect(20.0, 70.0, 30.0, 50.0)
            ]
        );
        // Canvas stacking: default first-on-top gives the first child the highest rank.
        let ranks = |r: &hsk_studio_components::AutoLayoutResult| {
            [2, 3, 4].map(|n| r.get(&id(n)).unwrap().paint_rank)
        };
        assert_eq!(ranks(&r), [2, 1, 0]);
        let mut reversed = tree(LayoutMode::Vertical, 10.0);
        reversed.layout.as_mut().unwrap().item_reverse_z_index = true;
        assert_eq!(ranks(&solve(&reversed, None).unwrap()), [0, 1, 2]);
        assert!(r.boxes.iter().all(|b| b.in_flow));
    });
}

#[test]
fn fill_clamps_space_between_and_resize() {
    bounded_case("fill_clamps_space_between_and_resize", || {
        // Fixed 300x100 row, padding left/right 10, gap 10. L 50 wide; G1 grows and stretches;
        // G2 grows with max_width 80.
        let mut l = AutoLayout::horizontal();
        (l.primary_axis_sizing, l.counter_axis_sizing) = (AxisSizing::Fixed, AxisSizing::Fixed);
        (l.padding_left, l.padding_right, l.item_spacing) = (10.0, 10.0, 10.0);
        let mut g2 = grow_h(leaf(4, 40.0, 20.0));
        g2.clamps = SizeClamps { max_width: Some(80.0), ..SizeClamps::default() };
        let root = container(
            leaf(1, 300.0, 100.0),
            l,
            vec![leaf(2, 50.0, 20.0), stretch_v(grow_h(leaf(3, 40.0, 20.0))), g2],
        );
        // Free space 300-20-50-20 = 210; equal share 105 > G2's max 80 => G2 = 80, G1 = 210-80 = 130.
        let r = solve(&root, None).unwrap();
        assert_eq!(
            boxes(&r, &[2, 3, 4]),
            vec![
                rect(10.0, 0.0, 50.0, 20.0),
                rect(70.0, 0.0, 130.0, 100.0),
                rect(210.0, 0.0, 80.0, 20.0)
            ]
        );
        // The same tree resized to 400 wide: free 310, share 155 => G2 80, G1 230, G2 x 70+230+10.
        let r = solve(&root, Some(SizePt { w: 400.0, h: 100.0 })).unwrap();
        assert_eq!(
            boxes(&r, &[1, 3, 4]),
            vec![
                rect(0.0, 0.0, 400.0, 100.0),
                rect(70.0, 0.0, 230.0, 100.0),
                rect(310.0, 0.0, 80.0, 20.0)
            ]
        );

        // SPACE_BETWEEN: inner 280, items 50+30+20 = 100 => gap (280-100)/2 = 90.
        let mut l = AutoLayout::horizontal();
        (l.primary_axis_sizing, l.counter_axis_sizing) = (AxisSizing::Fixed, AxisSizing::Fixed);
        (l.padding_left, l.padding_right) = (10.0, 10.0);
        l.primary_align = PrimaryAlign::SpaceBetween;
        l.counter_align = CounterAlign::Center;
        let sb = container(
            leaf(1, 300.0, 100.0),
            l,
            vec![leaf(2, 50.0, 20.0), leaf(3, 30.0, 40.0), leaf(4, 20.0, 60.0)],
        );
        let r = solve(&sb, None).unwrap();
        assert_eq!(
            boxes(&r, &[2, 3, 4]),
            vec![
                rect(10.0, 40.0, 50.0, 20.0),
                rect(150.0, 30.0, 30.0, 40.0),
                rect(270.0, 20.0, 20.0, 60.0)
            ]
        );
        // A min clamp above the equal share wins and the remainder goes to the other child.
        let mut l = AutoLayout::horizontal();
        (l.primary_axis_sizing, l.counter_axis_sizing) = (AxisSizing::Fixed, AxisSizing::Fixed);
        let mut a = grow_h(leaf(2, 10.0, 10.0));
        a.clamps.min_width = Some(150.0);
        let min_case = container(leaf(1, 200.0, 50.0), l, vec![a, grow_h(leaf(3, 10.0, 10.0))]);
        let r = solve(&min_case, None).unwrap();
        assert_eq!(boxes(&r, &[2, 3]), vec![rect(0.0, 0.0, 150.0, 10.0), rect(150.0, 0.0, 50.0, 10.0)]);
    });
}

#[test]
fn wrap_lines_and_typed_refusals() {
    bounded_case("wrap_lines_and_typed_refusals", || {
        // Wrap row 100 wide, gap 10, line gap 5, SPACE_BETWEEN, five 40x20 items: lines of 2,2,1.
        let mut l = AutoLayout::horizontal();
        l.wrap = WrapMode::Wrap;
        l.primary_axis_sizing = AxisSizing::Fixed;
        (l.item_spacing, l.counter_axis_spacing) = (10.0, Some(5.0));
        l.primary_align = PrimaryAlign::SpaceBetween;
        let items = || (2..=6).map(|n| leaf(n, 40.0, 20.0)).collect::<Vec<_>>();
        let wrap = container(leaf(1, 100.0, 0.0), l.clone(), items());
        // Line 1: gap (100-80)/1 = 20 => x 0 and 60; lines at y 0, 25, 50; hug height 3*20 + 2*5 = 70.
        let r = solve(&wrap, None).unwrap();
        assert_eq!(
            boxes(&r, &[1, 2, 3, 4, 5, 6]),
            vec![
                rect(0.0, 0.0, 100.0, 70.0),
                rect(0.0, 0.0, 40.0, 20.0),
                rect(60.0, 0.0, 40.0, 20.0),
                rect(0.0, 25.0, 40.0, 20.0),
                rect(60.0, 25.0, 40.0, 20.0),
                rect(0.0, 50.0, 40.0, 20.0)
            ]
        );
        // Fixed 100 tall + counter SPACE_BETWEEN: lines total 60, gap (100-60)/2 = 20 => y 0, 40, 80.
        l.counter_axis_sizing = AxisSizing::Fixed;
        l.align_content = AlignContent::SpaceBetween;
        let spaced = container(leaf(1, 100.0, 100.0), l, items());
        let r = solve(&spaced, None).unwrap();
        let ys: Vec<f64> = boxes(&r, &[2, 4, 6]).iter().map(|b| b.y).collect();
        assert_eq!(ys, vec![0.0, 40.0, 80.0]);

        // Typed refusals: recognised spec features are refused, never approximated.
        let row = |f: &dyn Fn(&mut AutoLayout)| {
            let mut l = AutoLayout::horizontal();
            f(&mut l);
            container(leaf(1, 0.0, 0.0), l, vec![leaf(2, 10.0, 10.0)])
        };
        let unsupported = |what: &str| Err(E::UnsupportedLayout(what.to_owned()));
        assert_eq!(solve(&row(&|l| l.mode = LayoutMode::Grid), None).map(|_| ()), unsupported("grid"));
        assert_eq!(
            solve(&row(&|l| l.counter_align = CounterAlign::Baseline), None).map(|_| ()),
            unsupported("baseline")
        );
        assert_eq!(
            solve(&row(&|l| l.strokes_included_in_layout = true), None).map(|_| ()),
            unsupported("strokes_included_in_layout")
        );
        assert_eq!(
            solve(&row(&|l| l.primary_align = PrimaryAlign::SpaceBetween), None).map(|_| ()),
            unsupported("space_between_with_hug")
        );
        assert_eq!(
            solve(&row(&|l| l.wrap = WrapMode::Wrap), None).map(|_| ()),
            unsupported("wrap_with_hug_primary")
        );
        // Sizing stated twice must agree; min above max is an error, not a silent clamp.
        let mut bad = row(&|_| {});
        bad.children[0].sizing_horizontal = ChildSizing::Fill; // FILL without layout_grow = 1
        assert_eq!(solve(&bad, None).map(|_| ()), Err(E::InconsistentSizing(id(2))));
        let mut bad = row(&|_| {});
        bad.sizing_horizontal = ChildSizing::Fixed; // container still says primary AUTO
        assert_eq!(solve(&bad, None).map(|_| ()), Err(E::InconsistentSizing(id(1))));
        let mut bad = row(&|_| {});
        bad.children[0].clamps = SizeClamps { min_width: Some(9.0), max_width: Some(5.0), ..SizeClamps::default() };
        assert_eq!(
            solve(&bad, None).map(|_| ()),
            Err(E::InvalidLayout(format!("min_greater_than_max@{}", id(2))))
        );
        let stop = CancellationToken::default();
        stop.cancel();
        assert_eq!(
            solve_auto_layout(&row(&|_| {}), None, &LayoutLimits::default(), &stop).map(|_| ()),
            Err(E::Canceled)
        );
    });
}

#[test]
fn nested_absolute_hug_fill_and_resize() {
    bounded_case("nested_absolute_hug_fill_and_resize", || {
        // Column 200 wide (fixed) hugging its height, padding 10, gap 8, authored height 150.
        //  H: row, FILL width (stretch), HUG height, centred items b1/b2 30x20 with gap 6.
        //  Body: 100x40, centred. Badge: absolute 16x16, pinned right/bottom (authored 200x150).
        let mut outer = AutoLayout::vertical().with_padding(10.0);
        outer.counter_axis_sizing = AxisSizing::Fixed;
        outer.item_spacing = 8.0;
        let mut header_layout = AutoLayout::horizontal();
        header_layout.primary_axis_sizing = AxisSizing::Fixed; // FILL width
        header_layout.primary_align = PrimaryAlign::Center;
        header_layout.item_spacing = 6.0;
        let mut header = container(
            leaf(2, 0.0, 0.0),
            header_layout,
            vec![leaf(3, 30.0, 20.0), leaf(4, 30.0, 20.0)],
        );
        header.sizing_horizontal = ChildSizing::Fill; // cross axis of the column
        header.align = ChildAlign::Stretch;
        let mut badge = leaf(6, 16.0, 16.0);
        badge.positioning = Positioning::Absolute;
        badge.placement = Some(Placement {
            x: 180.0,
            y: 130.0,
            constraints: Constraints { horizontal: ConstraintKind::Max, vertical: ConstraintKind::Max },
        });
        let root = container(
            leaf(1, 200.0, 150.0),
            outer,
            vec![header, aligned(leaf(5, 100.0, 40.0), ChildAlign::Center), badge],
        );
        // Height hugs 10 + 20 + 8 + 40 + 10 = 88 (the absolute badge does not count). H fills the
        // 180 inner width; its 66 of content is centred: start (180-66)/2 = 57, then 57+30+6 = 93.
        // Body x = 10 + (180-100)/2 = 50, y = 10+20+8 = 38. Badge keeps its 4 pt bottom offset:
        // y = 130 + (88-150) = 68, and its right offset x = 180 + (200-200) = 180.
        let r = solve(&root, None).unwrap();
        assert_eq!(
            boxes(&r, &[1, 2, 3, 4, 5, 6]),
            vec![
                rect(0.0, 0.0, 200.0, 88.0),
                rect(10.0, 10.0, 180.0, 20.0),
                rect(57.0, 0.0, 30.0, 20.0),
                rect(93.0, 0.0, 30.0, 20.0),
                rect(50.0, 38.0, 100.0, 40.0),
                rect(180.0, 68.0, 16.0, 16.0)
            ]
        );
        assert_eq!(r.get(&id(6)).map(|b| (b.in_flow, b.paint_rank)), Some((false, 0)));
        assert_eq!(r.get(&id(3)).map(|b| b.parent_id.clone()), Some(Some(id(2))));
        // Resize the outer frame to 300 wide: H = 280, content start (280-66)/2 = 107, badge x
        // 180 + (300-200) = 280; the hugged height stays 88 whatever height is requested.
        let r = solve(&root, Some(SizePt { w: 300.0, h: 999.0 })).unwrap();
        assert_eq!(
            boxes(&r, &[1, 2, 3, 4, 5, 6]),
            vec![
                rect(0.0, 0.0, 300.0, 88.0),
                rect(10.0, 10.0, 280.0, 20.0),
                rect(107.0, 0.0, 30.0, 20.0),
                rect(143.0, 0.0, 30.0, 20.0),
                rect(100.0, 38.0, 100.0, 40.0),
                rect(280.0, 68.0, 16.0, 16.0)
            ]
        );
        // Evaluation is deterministic and leaves its input untouched.
        let before = root.clone();
        assert_eq!(solve(&root, None).unwrap(), solve(&root, None).unwrap());
        assert_eq!(root, before);
    });
}
