//! Hand-computed auto-layout oracles (document unit = points) over Folio records. Ids are
//! `SLYR-0198a000-0000-7000-8000-<n hex>`; numbers below are the `n`. Every expected rectangle is
//! parent-local: (x, y, w, h). A test tree (`T`) is turned into a Folio document of group layers plus
//! `StudioDesignRecords` entries (auto layout, child layout, clamps, constraints) and the crate-local
//! authored rectangles; the solver reads only those.
use folio::{
    AxisSizingMode, ChildAlign, ChildPositioning, ChildSizing, ConstraintType, CounterAxisAlign,
    CounterAxisAlignContent, LayoutMode, LayoutWrap, NodeKind, NodeRef, Parent, PrimaryAxisAlign,
    StudioAutoLayout, StudioChildLayout, StudioConstraint, StudioSizeClamps,
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_components::{
    AutoLayoutResult, ComponentsError as E, Design, LayoutLimits, LocalContent, NodeLocal, RectPt,
    SizePt, solve_auto_layout,
};
use hsk_studio_folio as folio;
use std::collections::BTreeMap;

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
fn doc_id() -> String {
    "SDOC-0198a000-0000-7000-8000-000000000001".to_owned()
}
fn board_id() -> String {
    "SART-0198a000-0000-7000-8000-000000000001".to_owned()
}

// ---- test tree ---------------------------------------------------------------------------------------

/// One layout node as the tests describe it. Sizing is the child's `layout_sizing_*` pair; the root
/// of a tree has no child record (its sizing follows its own container modes).
#[derive(Clone)]
struct T {
    n: u32,
    w: f64,
    h: f64,
    sizing: (ChildSizing, ChildSizing),
    clamps: Option<StudioSizeClamps>,
    grow: u8,
    align: ChildAlign,
    positioning: ChildPositioning,
    /// Authored x, y and the constraints of a child that is not placed by flow.
    placement: Option<(f64, f64, StudioConstraint)>,
    layout: Option<StudioAutoLayout>,
    children: Vec<T>,
}
fn leaf(n: u32, w: f64, h: f64) -> T {
    T {
        n,
        w,
        h,
        sizing: (ChildSizing::Fixed, ChildSizing::Fixed),
        clamps: None,
        grow: 0,
        align: ChildAlign::Inherit,
        positioning: ChildPositioning::Auto,
        placement: None,
        layout: None,
        children: Vec::new(),
    }
}
fn flow(mode: LayoutMode) -> StudioAutoLayout {
    StudioAutoLayout {
        layout_mode: mode,
        ..StudioAutoLayout::none()
    }
}
fn horizontal() -> StudioAutoLayout {
    flow(LayoutMode::Horizontal)
}
fn vertical() -> StudioAutoLayout {
    flow(LayoutMode::Vertical)
}
fn padded(mut l: StudioAutoLayout, all: f64) -> StudioAutoLayout {
    (
        l.padding_top,
        l.padding_right,
        l.padding_bottom,
        l.padding_left,
    ) = (all, all, all, all);
    l
}
fn clamps() -> StudioSizeClamps {
    StudioSizeClamps {
        min_width: None,
        max_width: None,
        min_height: None,
        max_height: None,
    }
}
fn cons(horizontal: ConstraintType, vertical: ConstraintType) -> StudioConstraint {
    StudioConstraint {
        schema_id: folio::ConstraintSchema::V1,
        horizontal,
        vertical,
    }
}
/// Gives `node` the auto layout and the sizing modes that agree with it (AUTO = hug, FIXED = fixed).
fn container(mut node: T, layout: StudioAutoLayout, children: Vec<T>) -> T {
    let hug = |a: AxisSizingMode| {
        if a == AxisSizingMode::Auto {
            ChildSizing::Hug
        } else {
            ChildSizing::Fixed
        }
    };
    let (p, c) = (
        hug(layout.primary_axis_sizing_mode),
        hug(layout.counter_axis_sizing_mode),
    );
    node.sizing = if layout.layout_mode == LayoutMode::Horizontal {
        (p, c)
    } else {
        (c, p)
    };
    node.layout = Some(layout);
    node.children = children;
    node
}
fn aligned(mut n: T, a: ChildAlign) -> T {
    n.align = a;
    n
}
/// FILL on the flow (primary) axis of a horizontal parent: `layout_grow = 1`.
fn grow_h(mut n: T) -> T {
    n.sizing.0 = ChildSizing::Fill;
    n.grow = 1;
    n
}
/// FILL on the cross axis of a horizontal parent: STRETCH.
fn stretch_v(mut n: T) -> T {
    n.sizing.1 = ChildSizing::Fill;
    n.align = ChildAlign::Stretch;
    n
}

// ---- tree -> Folio records -----------------------------------------------------------------------------

struct Fixture {
    doc: folio::StudioDocument,
    records: folio::StudioDesignRecords,
    local: LocalContent,
}
impl Fixture {
    fn design(&self) -> Design<'_> {
        Design::new(&self.doc, &self.records, &self.local).unwrap()
    }
}

fn add(
    t: &T,
    parent: Parent,
    parent_in_flow: bool,
    layers: &mut Vec<folio::StudioLayer>,
    records: &mut folio::StudioDesignRecords,
    local: &mut LocalContent,
) {
    let node = NodeRef {
        node_kind: NodeKind::Layer,
        node_id: id(t.n),
    };
    layers.push(folio::StudioLayer {
        schema_id: folio::LayerSchema::V1,
        layer_id: id(t.n),
        name: format!("n{}", t.n),
        parent,
        kind: "group".into(),
        payload: folio::Payload::Group,
        visible: true,
        appearance: None,
        text_frame: None,
    });
    let (x, y) = t.placement.as_ref().map_or((0.0, 0.0), |p| (p.0, p.1));
    local.nodes.insert(
        id(t.n),
        NodeLocal {
            rect: Some(RectPt {
                x,
                y,
                w: t.w,
                h: t.h,
            }),
            ..NodeLocal::default()
        },
    );
    let child_layout = parent_in_flow.then_some(StudioChildLayout {
        layout_align: t.align,
        layout_grow: t.grow,
        layout_positioning: t.positioning,
        layout_sizing_horizontal: t.sizing.0,
        layout_sizing_vertical: t.sizing.1,
        grid_child: None,
    });
    let constraints = t.placement.as_ref().map(|p| p.2.clone());
    if t.layout.is_some() || child_layout.is_some() || t.clamps.is_some() || constraints.is_some() {
        records.nodes.push(folio::StudioNodeDesign {
            node: node.clone(),
            auto_layout: t.layout.clone(),
            child_layout,
            size_clamps: t.clamps.clone(),
            constraints,
            reactions: vec![],
            component_property_references: BTreeMap::new(),
            explicit_variable_modes: BTreeMap::new(),
            style_refs: BTreeMap::new(),
        });
    }
    let in_flow = t.layout.as_ref().is_some_and(|l| l.layout_mode.is_flow());
    for c in &t.children {
        add(
            c,
            Parent::Node(node.clone()),
            in_flow,
            layers,
            records,
            local,
        );
    }
}

/// The tree under `root` as a document (one artboard, group layers) plus its records.
fn world(root: &T) -> Fixture {
    let (mut layers, mut records, mut local) = (
        Vec::new(),
        folio::StudioDesignRecords::new(doc_id()),
        LocalContent::default(),
    );
    let board = Parent::Node(NodeRef {
        node_kind: NodeKind::Artboard,
        node_id: board_id(),
    });
    add(root, board, false, &mut layers, &mut records, &mut local);
    let len = |v: f64| folio::Length {
        value: v,
        unit: folio::GeometryUnit::Pt,
    };
    let doc = folio::StudioDocument {
        schema_id: folio::DocumentSchema::V1,
        document_id: doc_id(),
        revision: 1,
        geometry_unit: folio::GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: folio::ProfileBinding::Unassigned(()),
        working_space_cmyk: folio::ProfileBinding::Unassigned(()),
        working_space_gray: folio::ProfileBinding::Unassigned(()),
        working_space_spot: folio::ProfileBinding::Unassigned(()),
        blending_space: folio::ProfileBinding::Unassigned(()),
        artboards: vec![folio::StudioArtboard {
            schema_id: folio::ArtboardSchema::V1,
            artboard_id: board_id(),
            name: "Frame".into(),
            parent: Parent::Root(()),
            bounds: folio::Bounds {
                x: len(0.0),
                y: len(0.0),
                width: len(100.0),
                height: len(100.0),
            },
        }],
        page_spreads: vec![],
        layers,
        graph: folio::StudioLayerGraph {
            schema_id: folio::GraphSchema::V1,
            operations: vec![],
            edges: vec![],
            outputs: vec![],
        },
        sequences: vec![],
    };
    Fixture {
        doc,
        records,
        local,
    }
}
fn folio_result(fx: &Fixture) -> Result<Vec<folio::ResolutionIssue>, folio::Diagnostic> {
    folio::validate_design_records(
        &fx.records,
        &fx.doc,
        folio::Budget::default(),
        &CancellationToken::default(),
        &folio::Unresolved,
    )
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> RectPt {
    RectPt { x, y, w, h }
}
fn solve_raw(root: &T, size: Option<SizePt>) -> Result<AutoLayoutResult, E> {
    let fx = world(root);
    solve_auto_layout(
        &fx.design(),
        &id(root.n),
        size,
        &LayoutLimits::default(),
        &CancellationToken::default(),
    )
}
/// Solves after proving Folio accepts the document and the record set.
fn solve(root: &T, size: Option<SizePt>) -> Result<AutoLayoutResult, E> {
    let fx = world(root);
    let token = CancellationToken::default();
    folio::validate_document(
        fx.doc.clone(),
        folio::Budget::default(),
        &token,
        &folio::Unresolved,
    )
    .unwrap();
    assert_eq!(folio_result(&fx), Ok(vec![]));
    solve_auto_layout(
        &fx.design(),
        &id(root.n),
        size,
        &LayoutLimits::default(),
        &token,
    )
}
fn boxes(r: &AutoLayoutResult, ns: &[u32]) -> Vec<RectPt> {
    ns.iter().map(|n| r.get(&id(*n)).unwrap().rect).collect()
}

#[test]
fn flow_hug_gap_padding_alignment() {
    bounded_case("flow_hug_gap_padding_alignment", || {
        // Row, hug both axes, padding 10, gap 10. A 20x30 (min), B 40x10 (centre), C 30x50 (max).
        let tree = |mode: LayoutMode, gap: f64| {
            let mut l = padded(flow(mode), 10.0);
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
        let ranks = |r: &AutoLayoutResult| [2, 3, 4].map(|n| r.get(&id(n)).unwrap().paint_rank);
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
        let mut l = horizontal();
        (l.primary_axis_sizing_mode, l.counter_axis_sizing_mode) =
            (AxisSizingMode::Fixed, AxisSizingMode::Fixed);
        (l.padding_left, l.padding_right, l.item_spacing) = (10.0, 10.0, 10.0);
        let mut g2 = grow_h(leaf(4, 40.0, 20.0));
        g2.clamps = Some(StudioSizeClamps {
            max_width: Some(80.0),
            ..clamps()
        });
        let root = container(
            leaf(1, 300.0, 100.0),
            l,
            vec![
                leaf(2, 50.0, 20.0),
                stretch_v(grow_h(leaf(3, 40.0, 20.0))),
                g2,
            ],
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
        let mut l = horizontal();
        (l.primary_axis_sizing_mode, l.counter_axis_sizing_mode) =
            (AxisSizingMode::Fixed, AxisSizingMode::Fixed);
        (l.padding_left, l.padding_right) = (10.0, 10.0);
        l.primary_axis_align_items = PrimaryAxisAlign::SpaceBetween;
        l.counter_axis_align_items = CounterAxisAlign::Center;
        let sb = container(
            leaf(1, 300.0, 100.0),
            l,
            vec![
                leaf(2, 50.0, 20.0),
                leaf(3, 30.0, 40.0),
                leaf(4, 20.0, 60.0),
            ],
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
        let mut l = horizontal();
        (l.primary_axis_sizing_mode, l.counter_axis_sizing_mode) =
            (AxisSizingMode::Fixed, AxisSizingMode::Fixed);
        let mut a = grow_h(leaf(2, 10.0, 10.0));
        a.clamps = Some(StudioSizeClamps {
            min_width: Some(150.0),
            ..clamps()
        });
        let min_case = container(
            leaf(1, 200.0, 50.0),
            l,
            vec![a, grow_h(leaf(3, 10.0, 10.0))],
        );
        let r = solve(&min_case, None).unwrap();
        assert_eq!(
            boxes(&r, &[2, 3]),
            vec![rect(0.0, 0.0, 150.0, 10.0), rect(150.0, 0.0, 50.0, 10.0)]
        );
    });
}

#[test]
fn wrap_lines_and_typed_refusals() {
    bounded_case("wrap_lines_and_typed_refusals", || {
        // Wrap row 100 wide, gap 10, line gap 5, SPACE_BETWEEN, five 40x20 items: lines of 2,2,1.
        let mut l = horizontal();
        l.layout_wrap = LayoutWrap::Wrap;
        l.primary_axis_sizing_mode = AxisSizingMode::Fixed;
        (l.item_spacing, l.counter_axis_spacing) = (10.0, Some(5.0));
        l.primary_axis_align_items = PrimaryAxisAlign::SpaceBetween;
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
        l.counter_axis_sizing_mode = AxisSizingMode::Fixed;
        l.counter_axis_align_content = CounterAxisAlignContent::SpaceBetween;
        let spaced = container(leaf(1, 100.0, 100.0), l, items());
        let r = solve(&spaced, None).unwrap();
        let ys: Vec<f64> = boxes(&r, &[2, 4, 6]).iter().map(|b| b.y).collect();
        assert_eq!(ys, vec![0.0, 40.0, 80.0]);

        // Typed refusals: recognised spec features are refused, never approximated.
        let row = |f: &dyn Fn(&mut StudioAutoLayout)| {
            let mut l = horizontal();
            f(&mut l);
            container(leaf(1, 0.0, 0.0), l, vec![leaf(2, 10.0, 10.0)])
        };
        let unsupported = |what: &str| Err(E::UnsupportedLayout(what.to_owned()));
        // GRID is storable by no one: Folio refuses the record, and the evaluator refuses it too.
        let grid = row(&|l| l.layout_mode = LayoutMode::Grid);
        assert_eq!(
            folio_result(&world(&grid)).unwrap_err().code,
            folio::Code::UnsupportedDescriptor
        );
        assert_eq!(solve_raw(&grid, None).map(|_| ()), unsupported("grid"));
        assert_eq!(
            solve(
                &row(&|l| l.counter_axis_align_items = CounterAxisAlign::Baseline),
                None
            )
            .map(|_| ()),
            unsupported("baseline")
        );
        assert_eq!(
            solve(&row(&|l| l.strokes_included_in_layout = true), None).map(|_| ()),
            unsupported("strokes_included_in_layout")
        );
        assert_eq!(
            solve(
                &row(&|l| l.primary_axis_align_items = PrimaryAxisAlign::SpaceBetween),
                None
            )
            .map(|_| ()),
            unsupported("space_between_with_hug")
        );
        assert_eq!(
            solve(&row(&|l| l.layout_wrap = LayoutWrap::Wrap), None).map(|_| ()),
            unsupported("wrap_with_hug_primary")
        );
        // Sizing stated twice must agree. Folio stores both and enforces neither agreement.
        let mut bad = row(&|_| {});
        bad.children[0].sizing.0 = ChildSizing::Fill; // FILL without layout_grow = 1
        assert_eq!(
            solve(&bad, None).map(|_| ()),
            Err(E::InconsistentSizing(id(2)))
        );
        // A nested container whose child record says FIXED while its own primary axis is AUTO.
        let mut inner = row(&|_| {});
        inner.sizing.0 = ChildSizing::Fixed;
        let mut outer_layout = vertical();
        (
            outer_layout.primary_axis_sizing_mode,
            outer_layout.counter_axis_sizing_mode,
        ) = (AxisSizingMode::Fixed, AxisSizingMode::Fixed);
        let outer = container(leaf(9, 100.0, 100.0), outer_layout, vec![inner]);
        assert_eq!(
            solve(&outer, None).map(|_| ()),
            Err(E::InconsistentSizing(id(1)))
        );
        // Min above max is an error, not a silent clamp: Folio refuses the record and so does the solver.
        let mut bad = row(&|_| {});
        bad.children[0].clamps = Some(StudioSizeClamps {
            min_width: Some(9.0),
            max_width: Some(5.0),
            ..clamps()
        });
        assert_eq!(
            folio_result(&world(&bad)).unwrap_err().code,
            folio::Code::InvalidField
        );
        assert_eq!(
            solve_raw(&bad, None).map(|_| ()),
            Err(E::InvalidLayout(format!("min_greater_than_max@{}", id(2))))
        );
        let stop = CancellationToken::default();
        stop.cancel();
        let fx = world(&row(&|_| {}));
        assert_eq!(
            solve_auto_layout(&fx.design(), &id(1), None, &LayoutLimits::default(), &stop)
                .map(|_| ()),
            Err(E::Canceled)
        );
        // A node the document does not have, and a tree with no authored rectangle, are refused.
        assert_eq!(
            solve_auto_layout(
                &fx.design(),
                &id(77),
                None,
                &LayoutLimits::default(),
                &CancellationToken::default()
            )
            .map(|_| ()),
            Err(E::NoSuchNode(id(77)))
        );
        let mut no_rect = world(&row(&|_| {}));
        no_rect.local.nodes.remove(&id(2));
        assert_eq!(
            solve_auto_layout(
                &no_rect.design(),
                &id(1),
                None,
                &LayoutLimits::default(),
                &CancellationToken::default()
            )
            .map(|_| ()),
            Err(E::InvalidLayout(format!("rect_missing@{}", id(2))))
        );
    });
}

#[test]
fn nested_absolute_hug_fill_and_resize() {
    bounded_case("nested_absolute_hug_fill_and_resize", || {
        // Column 200 wide (fixed) hugging its height, padding 10, gap 8, authored height 150.
        //  H: row, FILL width (stretch), HUG height, centred items b1/b2 30x20 with gap 6.
        //  Body: 100x40, centred. Badge: absolute 16x16, pinned right/bottom (authored 200x150).
        let mut outer = padded(vertical(), 10.0);
        outer.counter_axis_sizing_mode = AxisSizingMode::Fixed;
        outer.item_spacing = 8.0;
        let mut header_layout = horizontal();
        header_layout.primary_axis_sizing_mode = AxisSizingMode::Fixed; // FILL width
        header_layout.primary_axis_align_items = PrimaryAxisAlign::Center;
        header_layout.item_spacing = 6.0;
        let mut header = container(
            leaf(2, 0.0, 0.0),
            header_layout,
            vec![leaf(3, 30.0, 20.0), leaf(4, 30.0, 20.0)],
        );
        header.sizing.0 = ChildSizing::Fill; // cross axis of the column
        header.align = ChildAlign::Stretch;
        let mut badge = leaf(6, 16.0, 16.0);
        badge.positioning = ChildPositioning::Absolute;
        badge.placement = Some((180.0, 130.0, cons(ConstraintType::Max, ConstraintType::Max)));
        let root = container(
            leaf(1, 200.0, 150.0),
            outer,
            vec![
                header,
                aligned(leaf(5, 100.0, 40.0), ChildAlign::Center),
                badge,
            ],
        );
        let fx = world(&root);
        folio::validate_document(
            fx.doc.clone(),
            folio::Budget::default(),
            &CancellationToken::default(),
            &folio::Unresolved,
        )
        .unwrap();
        assert_eq!(folio_result(&fx), Ok(vec![]));
        let design = fx.design();
        let (records0, local0) = (fx.records.clone(), fx.local.clone());
        let at = |size: Option<SizePt>| {
            solve_auto_layout(
                &design,
                &id(1),
                size,
                &LayoutLimits::default(),
                &CancellationToken::default(),
            )
        };
        // Height hugs 10 + 20 + 8 + 40 + 10 = 88 (the absolute badge does not count). H fills the
        // 180 inner width; its 66 of content is centred: start (180-66)/2 = 57, then 57+30+6 = 93.
        // Body x = 10 + (180-100)/2 = 50, y = 10+20+8 = 38. Badge keeps its 4 pt bottom offset:
        // y = 130 + (88-150) = 68, and its right offset x = 180 + (200-200) = 180.
        let r = at(None).unwrap();
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
        assert_eq!(
            r.get(&id(6)).map(|b| (b.in_flow, b.paint_rank)),
            Some((false, 0))
        );
        assert_eq!(
            r.get(&id(3)).map(|b| b.parent_id.clone()),
            Some(Some(id(2)))
        );
        // Resize the outer frame to 300 wide: H = 280, content start (280-66)/2 = 107, badge x
        // 180 + (300-200) = 280; the hugged height stays 88 whatever height is requested.
        let r = at(Some(SizePt { w: 300.0, h: 999.0 })).unwrap();
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
        assert_eq!(at(None).unwrap(), at(None).unwrap());
        assert_eq!((&fx.records, &fx.local), (&records0, &local0));
    });
}
