//! GRID auto layout (lane SMB-FIG-GRID-1, STU-DS-146..150): hand-computed track sizing and
//! placement oracles (document unit = points) over Folio records, and the three STU-DS-146
//! commands. Ids are `SLYR-0198a000-0000-7000-8000-<n hex>`; numbers below are the `n`. Expected
//! rectangles are parent-local (x, y, w, h).
use folio::{
    AxisSizingMode, ChildAlign, ChildPositioning, ChildSizing, ConstraintType, CounterAxisAlign,
    GridAutoTracks, GridItemsPositioning, LayoutMode, NodeKind, NodeRef, Parent, StudioAutoLayout,
    StudioChildLayout, StudioConstraint, StudioGridChild, StudioGridLayout, StudioGridTrackSize,
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_components::{
    AutoLayoutResult, ComponentsError as E, Design, GridTrackEntry, GridTrackMove, LayoutLimits,
    LocalContent, NodeLocal, RectPt, grid_append_child_at, grid_reorder_columns, grid_reorder_rows,
    solve_auto_layout,
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
const DOC: &str = "SDOC-0198a000-0000-7000-8000-000000000001";
const BOARD: &str = "SART-0198a000-0000-7000-8000-000000000001";

// ---- test tree ---------------------------------------------------------------------------------------

#[derive(Clone)]
struct N {
    n: u32,
    w: f64,
    h: f64,
    xy: (f64, f64),
    child: Option<StudioChildLayout>,
    constraints: Option<StudioConstraint>,
    layout: Option<StudioAutoLayout>,
    children: Vec<N>,
}
fn leaf(n: u32, w: f64, h: f64) -> N {
    N {
        n,
        w,
        h,
        xy: (0.0, 0.0),
        child: None,
        constraints: None,
        layout: None,
        children: Vec::new(),
    }
}
fn child_layout(h: ChildSizing, v: ChildSizing, align: ChildAlign) -> StudioChildLayout {
    StudioChildLayout {
        layout_align: align,
        layout_grow: 0,
        layout_positioning: ChildPositioning::Auto,
        layout_sizing_horizontal: h,
        layout_sizing_vertical: v,
        grid_child: None,
    }
}
/// A FIXED x FIXED in-flow child, optionally anchored.
fn item(n: u32, w: f64, h: f64, at: Option<(u32, u32, u32, u32)>) -> N {
    let mut c = leaf(n, w, h);
    let mut k = child_layout(ChildSizing::Fixed, ChildSizing::Fixed, ChildAlign::Inherit);
    k.grid_child = at.map(|(r, c, rs, cs)| StudioGridChild {
        grid_row_anchor_index: r,
        grid_column_anchor_index: c,
        grid_row_span: rs,
        grid_column_span: cs,
    });
    c.child = Some(k);
    c
}
fn grid_layout(
    rows: Vec<StudioGridTrackSize>,
    cols: Vec<StudioGridTrackSize>,
    gap: f64,
    pad: f64,
    width: Option<f64>,
) -> StudioAutoLayout {
    let mut l = StudioAutoLayout::none();
    l.layout_mode = LayoutMode::Grid;
    l.primary_axis_sizing_mode = if width.is_some() {
        AxisSizingMode::Fixed
    } else {
        AxisSizingMode::Auto
    };
    l.counter_axis_sizing_mode = AxisSizingMode::Auto;
    (l.padding_top, l.padding_right, l.padding_bottom, l.padding_left) = (pad, pad, pad, pad);
    l.grid = Some(StudioGridLayout {
        grid_row_count: u32::try_from(rows.len()).unwrap(),
        grid_column_count: u32::try_from(cols.len()).unwrap(),
        grid_row_gap: gap,
        grid_column_gap: gap,
        grid_row_sizes: rows,
        grid_column_sizes: cols,
        grid_auto_tracks: GridAutoTracks::None,
        grid_items_positioning: GridItemsPositioning::RowAutoFlow,
    });
    l
}
fn grid_mut(t: &mut N) -> &mut StudioGridLayout {
    t.layout.as_mut().unwrap().grid.as_mut().unwrap()
}
fn hug() -> StudioGridTrackSize {
    StudioGridTrackSize::hug()
}
/// A root grid frame: width fixed when the layout's primary axis is FIXED, height hugging.
fn root(l: StudioAutoLayout, w: f64, children: Vec<N>) -> N {
    let mut r = leaf(1, w, 0.0);
    r.layout = Some(l);
    r.children = children;
    r
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
fn add(t: &N, parent: Parent, layers: &mut Vec<folio::StudioLayer>, fx: &mut Fixture) {
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
    fx.local.nodes.insert(
        id(t.n),
        NodeLocal {
            rect: Some(RectPt {
                x: t.xy.0,
                y: t.xy.1,
                w: t.w,
                h: t.h,
            }),
            ..NodeLocal::default()
        },
    );
    if t.layout.is_some() || t.child.is_some() || t.constraints.is_some() {
        fx.records.nodes.push(folio::StudioNodeDesign {
            node: node.clone(),
            auto_layout: t.layout.clone(),
            child_layout: t.child.clone(),
            size_clamps: None,
            constraints: t.constraints.clone(),
            reactions: vec![],
            component_property_references: BTreeMap::new(),
            explicit_variable_modes: BTreeMap::new(),
            style_refs: BTreeMap::new(),
        });
    }
    for c in &t.children {
        add(c, Parent::Node(node.clone()), layers, fx);
    }
}
fn world(root: &N) -> Fixture {
    let len = |v: f64| folio::Length {
        value: v,
        unit: folio::GeometryUnit::Pt,
    };
    let mut fx = Fixture {
        doc: folio::StudioDocument {
            schema_id: folio::DocumentSchema::V1,
            document_id: DOC.to_owned(),
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
                artboard_id: BOARD.to_owned(),
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
            layers: vec![],
            graph: folio::StudioLayerGraph {
                schema_id: folio::GraphSchema::V1,
                operations: vec![],
                edges: vec![],
                outputs: vec![],
            },
            sequences: vec![],
        },
        records: folio::StudioDesignRecords::new(DOC.to_owned()),
        local: LocalContent::default(),
    };
    let mut layers = Vec::new();
    let board = Parent::Node(NodeRef {
        node_kind: NodeKind::Artboard,
        node_id: BOARD.to_owned(),
    });
    add(root, board, &mut layers, &mut fx);
    fx.doc.layers = layers;
    fx
}
fn folio_ok(fx: &Fixture) {
    assert_eq!(
        folio::validate_design_records(
            &fx.records,
            &fx.doc,
            folio::Budget::default(),
            &CancellationToken::default(),
            &folio::Unresolved,
        ),
        Ok(vec![])
    );
}
fn solve_fx(fx: &Fixture, root_n: u32) -> Result<AutoLayoutResult, E> {
    solve_auto_layout(
        &fx.design(),
        &id(root_n),
        None,
        &LayoutLimits::default(),
        &CancellationToken::default(),
    )
}
/// Solves after proving Folio accepts the record set.
fn solve(t: &N) -> Result<AutoLayoutResult, E> {
    let fx = world(t);
    folio_ok(&fx);
    solve_fx(&fx, t.n)
}
/// Solves without the Folio gate (hostile records the evaluator must refuse on its own).
fn solve_raw(t: &N) -> Result<AutoLayoutResult, E> {
    solve_fx(&world(t), t.n)
}
fn rect(x: f64, y: f64, w: f64, h: f64) -> RectPt {
    RectPt { x, y, w, h }
}
fn boxes(r: &AutoLayoutResult, ns: &[u32]) -> Vec<RectPt> {
    ns.iter().map(|n| r.get(&id(*n)).unwrap().rect).collect()
}
fn near(a: RectPt, b: RectPt) -> bool {
    [(a.x, b.x), (a.y, b.y), (a.w, b.w), (a.h, b.h)]
        .iter()
        .all(|(p, q)| (p - q).abs() < 1e-9)
}

// ---- solver ----------------------------------------------------------------------------------------------

#[test]
fn fixed_flex_columns_and_hug_rows_by_hand() {
    bounded_case("fixed_flex_columns_and_hug_rows_by_hand", || {
        // 2 x 3 grid, 500 wide, gap 10, padding 20; columns FIXED 100 / FLEX 1 / FLEX 2, rows HUG.
        // Inner width 460; FLEX leftover 460 - 100 - 2*10 = 340 -> 340/3 and 680/3.
        // Column x: 20, 20+100+10 = 130, 130 + 340/3 + 10. Rows hug the tallest child: row 0
        // max(30, 50, 20) = 50, row 1 max(40, 10, 10) = 40; row y: 20, 20+50+10 = 80; height
        // 20 + 50 + 10 + 40 + 20 = 140. Children are FIXED 50 wide, MIN aligned, auto-flowed.
        let cols = vec![
            StudioGridTrackSize::fixed(100.0),
            StudioGridTrackSize::flex(1.0),
            StudioGridTrackSize::flex(2.0),
        ];
        let heights = [30.0, 50.0, 20.0, 40.0, 10.0, 10.0];
        let kids = (0..6u32)
            .map(|i| item(2 + i, 50.0, heights[i as usize], None))
            .collect();
        let t = root(
            grid_layout(vec![hug(), hug()], cols, 10.0, 20.0, Some(500.0)),
            500.0,
            kids,
        );
        let r = solve(&t).unwrap();
        let x2 = 130.0 + 340.0 / 3.0 + 10.0;
        let want = [
            rect(0.0, 0.0, 500.0, 140.0),
            rect(20.0, 20.0, 50.0, 30.0),
            rect(130.0, 20.0, 50.0, 50.0),
            rect(x2, 20.0, 50.0, 20.0),
            rect(20.0, 80.0, 50.0, 40.0),
            rect(130.0, 80.0, 50.0, 10.0),
            rect(x2, 80.0, 50.0, 10.0),
        ];
        for (got, want) in boxes(&r, &[1, 2, 3, 4, 5, 6, 7]).into_iter().zip(want) {
            assert!(near(got, want), "{got:?} != {want:?}");
        }
        assert!(r.boxes.iter().all(|b| b.in_flow));
        // FILL children take the cell area: the FLEX 2 cell of row 0 is 680/3 x 50.
        let mut t2 = t.clone();
        t2.children[2].child.as_mut().unwrap().layout_sizing_horizontal = ChildSizing::Fill;
        let r = solve(&t2).unwrap();
        assert!(near(r.get(&id(4)).unwrap().rect, rect(x2, 20.0, 680.0 / 3.0, 20.0)));
    });
}

#[test]
fn spanning_child_and_auto_flow_skip_anchored_cells() {
    bounded_case("spanning_child_and_auto_flow_skip_anchored_cells", || {
        // 2 x 3 HUG grid, no gap or padding, ROW_AUTO_FLOW. Document order: A auto 10x10, B anchored
        // (0,1) 20x20, C auto 30x10, D anchored (1,0) spanning 2 columns 100x15, E auto 5x5.
        // Placement: A (0,0); C skips B's (0,1) -> (0,2); E skips D's (1,0) and (1,1) -> (1,2).
        // Columns from span-1 children: 10, 20, max(30, 5) = 30; D needs 100 over 10 + 20 = 30:
        // 70 more in equal shares -> 45, 55. Rows: max(10, 20, 10) = 20, max(15, 5) = 15.
        // x: 0, 45, 100; y: 0, 20; container 130 x 35.
        let kids = vec![
            item(2, 10.0, 10.0, None),
            item(3, 20.0, 20.0, Some((0, 1, 1, 1))),
            item(4, 30.0, 10.0, None),
            item(5, 100.0, 15.0, Some((1, 0, 1, 2))),
            item(6, 5.0, 5.0, None),
        ];
        let t = root(
            grid_layout(vec![hug(), hug()], vec![hug(), hug(), hug()], 0.0, 0.0, None),
            0.0,
            kids,
        );
        let r = solve(&t).unwrap();
        assert_eq!(
            boxes(&r, &[1, 2, 3, 4, 5, 6]),
            vec![
                rect(0.0, 0.0, 130.0, 35.0),
                rect(0.0, 0.0, 10.0, 10.0),
                rect(45.0, 0.0, 20.0, 20.0),
                rect(100.0, 0.0, 30.0, 10.0),
                rect(0.0, 20.0, 100.0, 15.0),
                rect(100.0, 20.0, 5.0, 5.0),
            ]
        );
        // MANUAL: every in-flow child needs anchors.
        let mut manual = t.clone();
        grid_mut(&mut manual).grid_items_positioning = GridItemsPositioning::Manual;
        assert_eq!(
            solve(&manual).map(|_| ()),
            Err(E::InvalidLayout(format!("grid_child_unanchored@{}", id(2))))
        );
    });
}

#[test]
fn auto_tracks_rows_adds_a_row_and_none_overflow_is_refused() {
    bounded_case("auto_tracks_rows_adds_a_row_and_none_overflow_is_refused", || {
        // 1 x 2 HUG grid, gap 4, three auto children: the third overflows.
        let kids = vec![
            item(2, 10.0, 10.0, None),
            item(3, 10.0, 20.0, None),
            item(4, 15.0, 7.0, None),
        ];
        let mut t = root(
            grid_layout(vec![hug()], vec![hug(), hug()], 4.0, 0.0, None),
            0.0,
            kids,
        );
        assert_eq!(
            solve(&t).map(|_| ()),
            Err(E::InvalidLayout(format!("grid_overflow@{}", id(1))))
        );
        // ROWS: an implicit HUG row (height 7) holds it at (1, 0). Columns max(10, 15) = 15, 10.
        // Rows 20, 7; y 0, 24; container 15 + 4 + 10 = 29 x 20 + 4 + 7 = 31.
        grid_mut(&mut t).grid_auto_tracks = GridAutoTracks::Rows;
        let r = solve(&t).unwrap();
        assert_eq!(
            boxes(&r, &[1, 2, 3, 4]),
            vec![
                rect(0.0, 0.0, 29.0, 31.0),
                rect(0.0, 0.0, 10.0, 10.0),
                rect(19.0, 0.0, 10.0, 20.0),
                rect(0.0, 24.0, 15.0, 7.0),
            ]
        );
    });
}

#[test]
fn grid_nested_in_a_horizontal_flow_aligns_fills_and_keeps_absolute_constraints() {
    bounded_case(
        "grid_nested_in_a_horizontal_flow_aligns_fills_and_keeps_absolute_constraints",
        || {
            // Row (hug, gap 0): X 40x10, then grid G (hug both axes): one HUG row, columns FIXED 30
            // and HUG; counter_axis_align_items CENTER. P fills its 30 wide cell (authored 5x8);
            // Q 20x4 centred in the 8 high row -> y 2. G = 50 x 8 at x 40; row 90 x 10. Z is an
            // ABSOLUTE child of G pinned MIN/MIN at (5, 5).
            let mut g = grid_layout(
                vec![hug()],
                vec![StudioGridTrackSize::fixed(30.0), hug()],
                0.0,
                0.0,
                None,
            );
            g.counter_axis_align_items = CounterAxisAlign::Center;
            let mut p = item(4, 5.0, 8.0, Some((0, 0, 1, 1)));
            p.child.as_mut().unwrap().layout_sizing_horizontal = ChildSizing::Fill;
            let q = item(5, 20.0, 4.0, Some((0, 1, 1, 1)));
            let mut z = leaf(6, 3.0, 3.0);
            z.xy = (5.0, 5.0);
            let mut zk = child_layout(ChildSizing::Fixed, ChildSizing::Fixed, ChildAlign::Inherit);
            zk.layout_positioning = ChildPositioning::Absolute;
            z.child = Some(zk);
            z.constraints = Some(StudioConstraint {
                schema_id: folio::ConstraintSchema::V1,
                horizontal: ConstraintType::Min,
                vertical: ConstraintType::Min,
            });
            let mut grid_node = leaf(3, 0.0, 0.0);
            grid_node.layout = Some(g);
            grid_node.child = Some(child_layout(
                ChildSizing::Hug,
                ChildSizing::Hug,
                ChildAlign::Inherit,
            ));
            grid_node.children = vec![p, q, z];
            let mut x = leaf(2, 40.0, 10.0);
            x.child = Some(child_layout(
                ChildSizing::Fixed,
                ChildSizing::Fixed,
                ChildAlign::Inherit,
            ));
            let mut row = StudioAutoLayout::none();
            row.layout_mode = LayoutMode::Horizontal;
            let t = root(row, 0.0, vec![x, grid_node]);
            let r = solve(&t).unwrap();
            assert_eq!(
                boxes(&r, &[1, 2, 3, 4, 5, 6]),
                vec![
                    rect(0.0, 0.0, 90.0, 10.0),
                    rect(0.0, 0.0, 40.0, 10.0),
                    rect(40.0, 0.0, 50.0, 8.0),
                    rect(0.0, 0.0, 30.0, 8.0),
                    rect(30.0, 2.0, 20.0, 4.0),
                    rect(5.0, 5.0, 3.0, 3.0),
                ]
            );
            assert!(!r.get(&id(6)).unwrap().in_flow);
        },
    );
}

#[test]
fn hostile_grid_input_is_refused_before_allocation() {
    bounded_case("hostile_grid_input_is_refused_before_allocation", || {
        let base = || {
            root(
                grid_layout(vec![hug()], vec![hug()], 0.0, 0.0, None),
                0.0,
                vec![item(2, 1.0, 1.0, None)],
            )
        };
        let refused = |t: &N, what: &str| {
            assert_eq!(
                solve_raw(t).map(|_| ()),
                Err(E::InvalidLayout(format!("{what}@{}", id(1)))),
                "{what}"
            );
        };
        // 1000 x 1000 counts with track lists far longer than the counts.
        let mut t = base();
        let g = grid_mut(&mut t);
        (g.grid_row_count, g.grid_column_count) = (1000, 1000);
        g.grid_row_sizes = vec![hug(); 1000];
        g.grid_column_sizes = vec![hug(); 2_000_000];
        refused(&t, "grid_tracks");
        // NaN and negative track values, NaN gaps, counts past 1000.
        let mut t = base();
        grid_mut(&mut t).grid_column_sizes = vec![StudioGridTrackSize::flex(f64::NAN)];
        refused(&t, "grid_tracks");
        let mut t = base();
        grid_mut(&mut t).grid_row_sizes = vec![StudioGridTrackSize::fixed(-1.0)];
        refused(&t, "grid_tracks");
        let mut t = base();
        grid_mut(&mut t).grid_row_gap = f64::NAN;
        refused(&t, "grid_gap");
        let mut t = base();
        grid_mut(&mut t).grid_row_count = u32::MAX;
        refused(&t, "grid_count");
        // Spans of u32::MAX never wrap the anchor + span sum.
        let mut t = base();
        t.children[0] = item(2, 1.0, 1.0, Some((1, 0, u32::MAX, 1)));
        assert_eq!(
            solve_raw(&t).map(|_| ()),
            Err(E::InvalidLayout(format!("grid_span@{}", id(2))))
        );
        // GRID without its record, a grid record under HORIZONTAL, a grid child in a flow.
        let mut t = base();
        t.layout.as_mut().unwrap().grid = None;
        refused(&t, "grid_fields");
        let mut t = base();
        t.layout.as_mut().unwrap().layout_mode = LayoutMode::Horizontal;
        refused(&t, "grid_fields");
        let mut t = base();
        let l = t.layout.as_mut().unwrap();
        (l.layout_mode, l.grid) = (LayoutMode::Horizontal, None);
        t.children[0] = item(2, 1.0, 1.0, Some((0, 0, 1, 1)));
        assert_eq!(
            solve_raw(&t).map(|_| ()),
            Err(E::InvalidLayout(format!("grid_child_outside_grid@{}", id(2))))
        );
        // A 1000 x 1000 grid with one anchored child covering every cell and one auto child:
        // the occupancy pass is bounded and the overflow is refused typed.
        let mut t = base();
        let g = grid_mut(&mut t);
        (g.grid_row_count, g.grid_column_count) = (1000, 1000);
        g.grid_row_sizes = vec![hug(); 1000];
        g.grid_column_sizes = vec![hug(); 1000];
        t.children = vec![
            item(2, 1.0, 1.0, Some((0, 0, 1000, 1000))),
            item(3, 1.0, 1.0, None),
        ];
        refused(&t, "grid_overflow");
    });
}

// ---- commands --------------------------------------------------------------------------------------------

/// 3 x 2 MANUAL grid: rows FIXED 10 / 20 / 30, columns HUG; A at (0,0), B at (2,1), C unanchored.
fn manual_grid() -> N {
    let mut l = grid_layout(
        vec![
            StudioGridTrackSize::fixed(10.0),
            StudioGridTrackSize::fixed(20.0),
            StudioGridTrackSize::fixed(30.0),
        ],
        vec![hug(), hug()],
        0.0,
        0.0,
        None,
    );
    l.grid.as_mut().unwrap().grid_items_positioning = GridItemsPositioning::Manual;
    let mut c = leaf(4, 5.0, 5.0);
    c.child = None;
    root(
        l,
        0.0,
        vec![
            item(2, 5.0, 5.0, Some((0, 0, 1, 1))),
            item(3, 5.0, 5.0, Some((2, 1, 1, 1))),
            c,
        ],
    )
}
fn anchors_of(r: &folio::StudioDesignRecords, n: u32) -> Option<StudioGridChild> {
    r.nodes
        .iter()
        .find(|d| d.node.node_id == id(n))
        .and_then(|d| d.child_layout.as_ref())
        .and_then(|k| k.grid_child)
}

#[test]
fn append_child_at_sets_the_anchors() {
    bounded_case("append_child_at_sets_the_anchors", || {
        let fx = world(&manual_grid());
        folio_ok(&fx);
        // C has no design entry: one is created with default child fields and the anchors.
        let records = grid_append_child_at(&fx.design(), &id(1), &id(4), 1, 1).unwrap();
        assert_eq!(
            anchors_of(&records, 4),
            Some(StudioGridChild {
                grid_row_anchor_index: 1,
                grid_column_anchor_index: 1,
                grid_row_span: 1,
                grid_column_span: 1,
            })
        );
        // The successor solves: rows 10/20/30 -> C at y 10, column 1 x 5.
        let next = Fixture {
            doc: fx.doc.clone(),
            records,
            local: fx.local.clone(),
        };
        let r = solve_fx(&next, 1).unwrap();
        assert_eq!(r.get(&id(4)).unwrap().rect, rect(5.0, 10.0, 5.0, 5.0));
        // Re-anchoring A keeps its spans.
        let records = grid_append_child_at(&fx.design(), &id(1), &id(2), 1, 0).unwrap();
        assert_eq!(anchors_of(&records, 2).unwrap().grid_row_anchor_index, 1);
    });
}

#[test]
fn reorder_rows_returns_ordered_tracks_and_moves_anchored_children() {
    bounded_case(
        "reorder_rows_returns_ordered_tracks_and_moves_anchored_children",
        || {
            let fx = world(&manual_grid());
            let moved = grid_reorder_rows(
                &fx.design(),
                &id(1),
                &GridTrackMove {
                    from_index: 0,
                    to_index: 2,
                },
            )
            .unwrap();
            let fixed = StudioGridTrackSize::fixed;
            assert_eq!(
                moved.tracks,
                vec![
                    GridTrackEntry {
                        index: 0,
                        previous_index: 1,
                        size: fixed(20.0)
                    },
                    GridTrackEntry {
                        index: 1,
                        previous_index: 2,
                        size: fixed(30.0)
                    },
                    GridTrackEntry {
                        index: 2,
                        previous_index: 0,
                        size: fixed(10.0)
                    },
                ]
            );
            // A moves with row 0 to row 2; B's row 2 becomes row 1.
            assert_eq!(anchors_of(&moved.records, 2).unwrap().grid_row_anchor_index, 2);
            assert_eq!(anchors_of(&moved.records, 3).unwrap().grid_row_anchor_index, 1);
            let stored = moved.records.nodes[0].auto_layout.as_ref().unwrap();
            assert_eq!(
                stored.grid.as_ref().unwrap().grid_row_sizes,
                vec![fixed(20.0), fixed(30.0), fixed(10.0)]
            );
            // Columns: B moves from column 1 to 0, A from 0 to 1.
            let cols = grid_reorder_columns(
                &fx.design(),
                &id(1),
                &GridTrackMove {
                    from_index: 1,
                    to_index: 0,
                },
            )
            .unwrap();
            assert_eq!(cols.tracks[0].previous_index, 1);
            assert_eq!(anchors_of(&cols.records, 3).unwrap().grid_column_anchor_index, 0);
            assert_eq!(anchors_of(&cols.records, 2).unwrap().grid_column_anchor_index, 1);
        },
    );
}

#[test]
fn command_refusals_change_nothing() {
    bounded_case("command_refusals_change_nothing", || {
        let mut t = manual_grid();
        // B spans rows 1..=2 so a move between them would split it.
        t.children[1] = item(3, 5.0, 5.0, Some((1, 1, 2, 1)));
        let fx = world(&t);
        folio_ok(&fx);
        let before = fx.records.clone();
        let d = fx.design();
        let invalid = |what: &str, n: u32| Err(E::InvalidLayout(format!("{what}@{}", id(n))));
        let refused = |what: &str, n: u32| E::InvalidLayout(format!("{what}@{}", id(n)));
        // Out of range: row 3 of 3, and an anchor that leaves no room for B's 2-row span.
        assert_eq!(grid_append_child_at(&d, &id(1), &id(4), 3, 0), Err(refused("grid_out_of_range", 4)));
        assert_eq!(grid_append_child_at(&d, &id(1), &id(3), 2, 1), Err(refused("grid_out_of_range", 3)));
        // Occupied under MANUAL: A holds (0,0).
        assert_eq!(grid_append_child_at(&d, &id(1), &id(4), 0, 0), Err(refused("grid_cell_occupied", 4)));
        // Not a GRID container; not a child of the container.
        assert_eq!(grid_append_child_at(&d, &id(2), &id(4), 0, 0), Err(refused("grid_not_a_grid", 2)));
        assert_eq!(
            grid_append_child_at(&d, &id(1), &id(1), 1, 0),
            Err(E::NoSuchNode(id(1)))
        );
        let mv = |from_index, to_index| GridTrackMove {
            from_index,
            to_index,
        };
        assert_eq!(
            grid_reorder_rows(&d, &id(1), &mv(0, 3)).map(|_| ()),
            invalid("grid_out_of_range", 1)
        );
        assert_eq!(
            grid_reorder_rows(&d, &id(1), &mv(1, 2)).map(|_| ()),
            invalid("grid_reorder_splits_span", 3)
        );
        assert_eq!(
            grid_reorder_columns(&d, &id(2), &mv(0, 0)).map(|_| ()),
            invalid("grid_not_a_grid", 2)
        );
        assert_eq!(fx.records, before);
    });
}
