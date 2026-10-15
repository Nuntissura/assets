//! Lane SMB-ILLUSTRATOR-PEN slice 1: the editable anchor path model (pen, anchor convert, node
//! select, scissors, join, reverse) against hand-built geometry.
use hsk_studio_nib::anchor_path::{
    AnchorAddr, AnchorError, AnchorPath, AnchorSelection, Constraint, Contour, EditAnchor, End, EndAddr, HandleSide,
    MAX_ANCHORS, PointType, append_anchor, bend_segment, close_contour, convert_anchor, delete_anchor, hit_anchor,
    hit_segment, insert_anchor, join, move_anchor, move_handle, point_at, reverse, snap45, split_at, split_at_anchor,
    start_contour,
};
use hsk_studio_nib::{FillRule, Mirroring, OwnedPath, PathElement as E, PathPoint};

fn p(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}

fn at(contour: usize, anchor: usize) -> AnchorAddr {
    AnchorAddr { contour, anchor }
}

fn open(anchors: Vec<EditAnchor>) -> AnchorPath {
    AnchorPath { contours: vec![Contour { anchors, closed: false }], fill_rule: FillRule::NonZero }
}

/// The arch (0,0) -> (10,0) with control points (0,10) and (10,10).
fn arch() -> AnchorPath {
    let mut a = EditAnchor::corner(0.0, 0.0);
    a.outgoing = p(0.0, 10.0);
    let mut b = EditAnchor::corner(10.0, 0.0);
    b.incoming = p(0.0, 10.0);
    open(vec![a, b])
}

#[test]
fn pen_builds_paths_and_owned_form_round_trips() {
    // Pen: click (corner), drag (smooth, mirrored handles), click, close on the first anchor.
    let path = start_contour(&AnchorPath::new(FillRule::NonZero), EditAnchor::corner(0.0, 0.0)).unwrap();
    let path = append_anchor(&path, 0, EditAnchor::smooth(10.0, 0.0, 0.0, 5.0)).unwrap();
    let path = append_anchor(&path, 0, EditAnchor::corner(10.0, 10.0)).unwrap();
    let closed = close_contour(&path, 0, None).unwrap();
    assert_eq!(close_contour(&closed, 0, None), Err(AnchorError::ContourClosed));
    assert_eq!(append_anchor(&closed, 0, EditAnchor::corner(1.0, 1.0)), Err(AnchorError::ContourClosed));
    let owned = closed.to_owned_path().unwrap();
    assert_eq!(
        owned.elements,
        vec![
            E::MoveTo(p(0.0, 0.0)),
            E::CubicTo(p(0.0, 0.0), p(10.0, -5.0), p(10.0, 0.0)),
            E::CubicTo(p(10.0, 5.0), p(10.0, 10.0), p(10.0, 10.0)),
            E::Close,
        ]
    );
    // Back to anchors: same geometry, mirroring inferred (the smooth anchor is ANGLE_AND_LENGTH).
    let back = AnchorPath::from_owned_path(&owned).unwrap();
    assert_eq!(back, closed);
    assert_eq!(back.anchor(at(0, 1)).unwrap().point_type(), PointType::Smooth);
    assert_eq!(back.anchor(at(0, 0)).unwrap().point_type(), PointType::Corner);
    // A closed square whose last LineTo returns to the start merges the duplicate anchor; a quad is raised.
    let q = OwnedPath {
        elements: vec![E::MoveTo(p(0.0, 0.0)), E::QuadTo(p(3.0, 6.0), p(6.0, 0.0)), E::LineTo(p(0.0, 0.0)), E::Close],
        fill_rule: FillRule::EvenOdd,
    };
    let a = AnchorPath::from_owned_path(&q).unwrap();
    assert_eq!(a.contours[0].anchors.len(), 2);
    assert!(a.contours[0].closed);
    assert_eq!(a.contours[0].anchors[0].outgoing, p(2.0, 4.0));
    assert_eq!(a.contours[0].anchors[1].incoming, p(-2.0, 4.0));
    assert_eq!(a.fill_rule, FillRule::EvenOdd);
}

#[test]
fn insert_delete_convert_hand_values() {
    // de Casteljau at t = 0.5: M = (5, 7.5), new handles (-2.5, 0) and (2.5, 0); outer handles halve.
    let path = insert_anchor(&arch(), 0, 0, 0.5).unwrap();
    let c = &path.contours[0].anchors;
    assert_eq!(c.len(), 3);
    assert_eq!(c[0].outgoing, p(0.0, 5.0));
    assert_eq!(c[1].position, p(5.0, 7.5));
    assert_eq!((c[1].incoming, c[1].outgoing), (p(-2.5, 0.0), p(2.5, 0.0)));
    assert_eq!(c[1].mirroring, Mirroring::Angle);
    assert_eq!(c[2].incoming, p(0.0, 5.0));
    // The curve is unchanged: the original midpoint is the new anchor, quarter points agree.
    assert_eq!(point_at(&arch(), 0, 0, 0.25).unwrap(), point_at(&path, 0, 0, 0.5).unwrap());
    // Convert to corner retracts handles; back to smooth on a straight chord synthesises chord/3 handles.
    let corner = convert_anchor(&path, at(0, 1), Mirroring::None).unwrap();
    assert_eq!(corner.contours[0].anchors[1].incoming, p(0.0, 0.0));
    assert_eq!(corner.contours[0].anchors[1].point_type(), PointType::Corner);
    let line = open(vec![EditAnchor::corner(0.0, 0.0), EditAnchor::corner(3.0, 3.0), EditAnchor::corner(6.0, 0.0)]);
    let smooth = convert_anchor(&line, at(0, 1), Mirroring::AngleAndLength).unwrap();
    let s = smooth.contours[0].anchors[1];
    // Chord (0,0)->(6,0): direction (1,0); neighbour distances sqrt(18)/3 each.
    let h = 18.0_f64.sqrt() / 3.0;
    assert_eq!((s.incoming, s.outgoing), (p(-h, 0.0), p(h, 0.0)));
    // Delete keeps the outer handles; deleting down to nothing is a typed empty result.
    let deleted = delete_anchor(&path, at(0, 1)).unwrap();
    assert_eq!(deleted.contours[0].anchors[0].outgoing, p(0.0, 5.0));
    let single = open(vec![EditAnchor::corner(1.0, 1.0)]);
    assert_eq!(delete_anchor(&single, at(0, 0)), Err(AnchorError::EmptyResult));
    // A straight segment gains a corner anchor at the lerp.
    let mid = insert_anchor(&line, 0, 0, 0.5).unwrap();
    assert_eq!(mid.contours[0].anchors[1], EditAnchor::corner(1.5, 1.5));
}

#[test]
fn move_anchor_handle_bend_and_constraints() {
    // Smooth (ANGLE_AND_LENGTH): the opposite handle mirrors exactly.
    let path = open(vec![EditAnchor::corner(0.0, 0.0), EditAnchor::smooth(10.0, 0.0, 4.0, 0.0), EditAnchor::corner(20.0, 0.0)]);
    let moved = move_handle(&path, at(0, 1), HandleSide::Out, p(3.0, 4.0), Constraint::Free, false).unwrap();
    let a = moved.contours[0].anchors[1];
    assert_eq!((a.outgoing, a.incoming), (p(3.0, 4.0), p(-3.0, -4.0)));
    // ANGLE keeps the opposite length (4) and points it opposite: (-3,-4)/5*4.
    let angle = convert_anchor(&path, at(0, 1), Mirroring::Angle).unwrap();
    let a = move_handle(&angle, at(0, 1), HandleSide::Out, p(3.0, 4.0), Constraint::Free, false).unwrap().contours[0].anchors[1];
    assert_eq!(a.incoming, p(-2.4000000000000004, -3.2));
    // Alt breaks the mirroring.
    let broke = move_handle(&path, at(0, 1), HandleSide::In, p(0.0, 2.0), Constraint::Free, true).unwrap().contours[0].anchors[1];
    assert_eq!((broke.incoming, broke.outgoing, broke.mirroring), (p(0.0, 2.0), p(4.0, 0.0), Mirroring::None));
    // Shift snaps the handle direction to 45 degrees, keeping the length 5.
    let snapped = move_handle(&path, at(0, 1), HandleSide::Out, p(3.0, 4.0), Constraint::Angle45, false).unwrap().contours[0].anchors[1];
    let d = 5.0 * 0.5_f64.sqrt();
    assert_eq!(snapped.outgoing, p(d, d));
    assert_eq!(snap45(p(10.0, 1.0)), p(101.0_f64.sqrt(), 0.0));
    // Moving an anchor carries its handles; Shift constrains the drag.
    let m = move_anchor(&path, at(0, 1), 2.0, 9.0, Constraint::Angle45).unwrap().contours[0].anchors[1];
    assert_eq!(m.position, p(10.0, 85.0_f64.sqrt()));
    assert_eq!(m.outgoing, p(4.0, 0.0));
    // Drag-to-bend: the midpoint of the straight (0,0)-(30,0) moves exactly to (15, 6).
    let line = open(vec![EditAnchor::corner(0.0, 0.0), EditAnchor::corner(30.0, 0.0)]);
    let bent = bend_segment(&line, 0, 0, 0.5, 0.0, 6.0).unwrap();
    let mid = point_at(&bent, 0, 0, 0.5).unwrap();
    assert!((mid.x - 15.0).abs() < 1e-12 && (mid.y - 6.0).abs() < 1e-12, "{mid:?}");
    // k = 1 / (3 u t (u^2 + t^2)) = 8/3 at t = 0.5: each handle moves by d * 4/3 = (0, 8).
    let h = bent.contours[0].anchors[0].outgoing;
    assert!(h.x == 0.0 && (h.y - 8.0).abs() < 1e-12, "{h:?}");
    // Off-centre t = 0.25 lands on target too.
    let bent = bend_segment(&line, 0, 0, 0.25, 0.0, 6.0).unwrap();
    let q = point_at(&bent, 0, 0, 0.25).unwrap();
    assert!((q.x - 7.5).abs() < 1e-12 && (q.y - 6.0).abs() < 1e-12, "{q:?}");
    // Hit tests: the handle end, the anchor, a segment point.
    let hit = hit_anchor(&path, p(14.2, 0.1), 0.5).unwrap();
    assert_eq!((hit.at, hit.part), (at(0, 1), AnchorSelection::OutHandle));
    assert_eq!(hit_anchor(&path, p(0.1, 0.0), 0.5).unwrap().part, AnchorSelection::Anchor);
    let seg = hit_segment(&line, p(12.0, 0.4), 1.0).unwrap();
    assert_eq!((seg.contour, seg.segment), (0, 0));
    // Retracted handles make the straight segment's parameter non-linear: check the point, not t.
    let q = point_at(&line, 0, 0, seg.t).unwrap();
    assert!((q.x - 12.0).abs() < 1e-6 && q.y == 0.0 && (seg.distance - 0.4).abs() < 1e-6, "{seg:?} {q:?}");
    assert!(hit_segment(&line, p(12.0, 5.0), 1.0).is_none());
}

#[test]
fn join_split_reverse() {
    let a = vec![EditAnchor::corner(0.0, 0.0), EditAnchor::corner(10.0, 0.0)];
    let b = vec![EditAnchor::corner(10.0, 0.0), EditAnchor::corner(10.0, 10.0)];
    let two = AnchorPath {
        contours: vec![Contour { anchors: a.clone(), closed: false }, Contour { anchors: b, closed: false }],
        fill_rule: FillRule::NonZero,
    };
    // Coincident ends merge: (0,0) (10,0) (10,10).
    let joined = join(&two, EndAddr { contour: 0, end: End::End }, EndAddr { contour: 1, end: End::Start }, 1e-6).unwrap();
    assert_eq!(joined.contours.len(), 1);
    let pos: Vec<PathPoint> = joined.contours[0].anchors.iter().map(|a| a.position).collect();
    assert_eq!(pos, vec![p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0)]);
    // Joining the start of one to the start of the other reverses the first: (10,0)(0,0) + straight + (10,0)(10,10).
    let joined = join(&two, EndAddr { contour: 0, end: End::Start }, EndAddr { contour: 1, end: End::Start }, 1e-6).unwrap();
    let pos: Vec<PathPoint> = joined.contours[0].anchors.iter().map(|a| a.position).collect();
    assert_eq!(pos, vec![p(10.0, 0.0), p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0)]);
    // Own ends close the contour; joining a closed contour or one end twice is refused.
    let closed = join(&joined, EndAddr { contour: 0, end: End::Start }, EndAddr { contour: 0, end: End::End }, 1e-6).unwrap();
    assert!(closed.contours[0].closed);
    let e = EndAddr { contour: 0, end: End::End };
    assert_eq!(join(&closed, e, EndAddr { contour: 0, end: End::Start }, 1e-6), Err(AnchorError::CannotJoinPathPoints));
    assert_eq!(join(&two, e, e, 1e-6), Err(AnchorError::CannotJoinPathPoints));
    assert_eq!(join(&two, e, EndAddr { contour: 9, end: End::Start }, 1e-6), Err(AnchorError::PathIndexOutOfBounds));
    // Scissors at a parametric point of the arch: two open contours sharing the coincident (5, 7.5).
    let cut = split_at(&arch(), 0, 0, 0.5).unwrap();
    assert_eq!(cut.contours.len(), 2);
    assert_eq!(cut.contours[0].anchors.last().unwrap().position, p(5.0, 7.5));
    assert_eq!(cut.contours[1].anchors[0].position, p(5.0, 7.5));
    assert_eq!(cut.contours[0].anchors.last().unwrap().incoming, p(-2.5, 0.0));
    assert_eq!(cut.contours[1].anchors[0].outgoing, p(2.5, 0.0));
    // Scissors on a closed square opens it at the anchor: 5 anchors, both ends at (10,0).
    let square = AnchorPath {
        contours: vec![Contour {
            anchors: vec![EditAnchor::corner(0.0, 0.0), EditAnchor::corner(10.0, 0.0), EditAnchor::corner(10.0, 10.0), EditAnchor::corner(0.0, 10.0)],
            closed: true,
        }],
        fill_rule: FillRule::NonZero,
    };
    let opened = split_at_anchor(&square, at(0, 1)).unwrap();
    let pos: Vec<PathPoint> = opened.contours[0].anchors.iter().map(|a| a.position).collect();
    assert_eq!(pos, vec![p(10.0, 0.0), p(10.0, 10.0), p(0.0, 10.0), p(0.0, 0.0), p(10.0, 0.0)]);
    assert!(!opened.contours[0].closed);
    assert_eq!(split_at_anchor(&arch(), at(0, 0)), Err(AnchorError::InvalidParameter));
    // Reverse: closed keeps its start, handles swap; reversing twice is the identity.
    let r = reverse(&square, None).unwrap();
    let pos: Vec<PathPoint> = r.contours[0].anchors.iter().map(|a| a.position).collect();
    assert_eq!(pos, vec![p(0.0, 0.0), p(0.0, 10.0), p(10.0, 10.0), p(10.0, 0.0)]);
    let ra = reverse(&arch(), Some(0)).unwrap();
    assert_eq!(ra.contours[0].anchors[0].position, p(10.0, 0.0));
    assert_eq!(ra.contours[0].anchors[0].outgoing, p(0.0, 10.0));
    assert_eq!(reverse(&ra, Some(0)).unwrap(), arch());
    assert_eq!(point_at(&ra, 0, 0, 0.25).unwrap(), point_at(&arch(), 0, 0, 0.75).unwrap());
}

#[test]
fn hostile_inputs_are_typed() {
    let nan = open(vec![EditAnchor::corner(f64::NAN, 0.0), EditAnchor::corner(1.0, 0.0)]);
    assert_eq!(nan.validate(), Err(AnchorError::NonFinite));
    assert_eq!(nan.to_owned_path(), Err(AnchorError::NonFinite));
    let far = open(vec![EditAnchor::corner(1e9, 0.0), EditAnchor::smooth(0.0, 0.0, 0.0, 0.0)]);
    let mut beyond = far.clone();
    beyond.contours[0].anchors[0].outgoing = p(1.0, 0.0);
    assert_eq!(beyond.validate(), Err(AnchorError::OutOfRange));
    assert_eq!(move_handle(&arch(), at(0, 0), HandleSide::Out, p(f64::INFINITY, 0.0), Constraint::Free, false), Err(AnchorError::NonFinite));
    assert_eq!(move_anchor(&arch(), at(0, 0), f64::NAN, 0.0, Constraint::Free), Err(AnchorError::NonFinite));
    assert_eq!(move_anchor(&arch(), at(0, 0), 2e9, 0.0, Constraint::Free), Err(AnchorError::OutOfRange));
    // Indices: STU-VEC-133 path/point index codes are distinct.
    assert_eq!(delete_anchor(&arch(), at(3, 0)), Err(AnchorError::PathIndexOutOfBounds));
    assert_eq!(delete_anchor(&arch(), at(0, 7)), Err(AnchorError::PointIndexOutOfBounds));
    assert_eq!(insert_anchor(&arch(), 0, 1, 0.5), Err(AnchorError::PointIndexOutOfBounds));
    assert_eq!(AnchorError::PathIndexOutOfBounds.code(), "path_index_out_of_bounds");
    assert_eq!(AnchorError::PointIndexOutOfBounds.code(), "point_index_out_of_bounds");
    assert_eq!(AnchorError::CannotJoinPathPoints.code(), "cannot_join_path_points");
    // Parameters.
    for t in [0.0, 1.0, -0.5, 2.0] {
        assert_eq!(insert_anchor(&arch(), 0, 0, t), Err(AnchorError::InvalidParameter));
        assert_eq!(bend_segment(&arch(), 0, 0, t, 1.0, 1.0), Err(AnchorError::InvalidParameter));
    }
    assert_eq!(insert_anchor(&arch(), 0, 0, f64::NAN), Err(AnchorError::NonFinite));
    // Degenerate: an empty contour, a one-anchor closed contour.
    let empty = AnchorPath { contours: vec![Contour { anchors: vec![], closed: false }], fill_rule: FillRule::NonZero };
    assert_eq!(empty.validate(), Err(AnchorError::Degenerate));
    let one = AnchorPath { contours: vec![Contour { anchors: vec![EditAnchor::corner(0.0, 0.0)], closed: true }], fill_rule: FillRule::NonZero };
    assert_eq!(one.validate(), Err(AnchorError::Degenerate));
    // Huge anchor counts: refused by validation before any operation allocates its result.
    let huge = open(vec![EditAnchor::corner(0.0, 0.0); MAX_ANCHORS + 1]);
    assert_eq!(huge.validate(), Err(AnchorError::BudgetExceeded));
    assert_eq!(insert_anchor(&huge, 0, 0, 0.5), Err(AnchorError::BudgetExceeded));
    let full = open(vec![EditAnchor::corner(0.0, 0.0); MAX_ANCHORS]);
    assert_eq!(append_anchor(&full, 0, EditAnchor::corner(1.0, 1.0)), Err(AnchorError::BudgetExceeded));
    assert_eq!(split_at_anchor(&full, at(0, 5)), Err(AnchorError::BudgetExceeded));
    // Converting a lone anchor to smooth has no direction to synthesise.
    let lone = open(vec![EditAnchor::corner(0.0, 0.0)]);
    assert_eq!(convert_anchor(&lone, at(0, 0), Mirroring::AngleAndLength), Err(AnchorError::Degenerate));
}
