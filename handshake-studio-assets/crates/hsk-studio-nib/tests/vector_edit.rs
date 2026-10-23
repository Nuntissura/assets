//! Lane SMB-NIB-VECTOR-RENDER slice 4: path editing primitives (insert/delete anchor, convert
//! corner/smooth, scissors, join, average, offset path).
use hsk_studio_nib::boolean::BooleanOptions;
use hsk_studio_nib::edit::{
    AnchorKind, AnchorRef, Axis, End, EndRef, average_anchors, convert_anchor, delete_anchor,
    insert_anchor, join, offset_path, split_at, split_at_anchor,
};
use hsk_studio_nib::stroke::LineJoin;
use hsk_studio_nib::{Error, FillRule, OwnedPath, PathElement as E, PathPoint};

fn p(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}

fn path(elements: Vec<E>) -> OwnedPath {
    OwnedPath { elements, fill_rule: FillRule::NonZero }
}

fn at(subpath: usize, anchor: usize) -> AnchorRef {
    AnchorRef { subpath, anchor }
}

fn square() -> OwnedPath {
    path(vec![E::MoveTo(p(0.0, 0.0)), E::LineTo(p(10.0, 0.0)), E::LineTo(p(10.0, 10.0)), E::LineTo(p(0.0, 10.0)), E::Close])
}

fn area(path: &OwnedPath) -> f64 {
    let mut total = 0.0;
    let mut pts: Vec<PathPoint> = Vec::new();
    for e in &path.elements {
        match *e {
            E::MoveTo(q) => pts = vec![q],
            E::LineTo(q) => pts.push(q),
            E::Close => {
                let n = pts.len();
                total += (0..n).map(|i| pts[i].x * pts[(i + 1) % n].y - pts[(i + 1) % n].x * pts[i].y).sum::<f64>() / 2.0;
            }
            _ => panic!("polygon expected"),
        }
    }
    total
}

#[test]
fn insert_and_delete_anchor() {
    let line = path(vec![E::MoveTo(p(0.0, 0.0)), E::LineTo(p(10.0, 0.0))]);
    let split = insert_anchor(&line, 0, 0, 0.25).unwrap();
    assert_eq!(split.elements, vec![E::MoveTo(p(0.0, 0.0)), E::LineTo(p(2.5, 0.0)), E::LineTo(p(10.0, 0.0))]);
    assert_eq!(delete_anchor(&split, at(0, 1)).unwrap(), line);
    // Cubic split at t = 0.5 (hand de Casteljau): (0,0) (0,8) (8,8) (8,0) -> midpoint (4,6).
    let cubic = path(vec![E::MoveTo(p(0.0, 0.0)), E::CubicTo(p(0.0, 8.0), p(8.0, 8.0), p(8.0, 0.0))]);
    let halves = insert_anchor(&cubic, 0, 0, 0.5).unwrap();
    assert_eq!(
        halves.elements,
        vec![
            E::MoveTo(p(0.0, 0.0)),
            E::CubicTo(p(0.0, 4.0), p(2.0, 6.0), p(4.0, 6.0)),
            E::CubicTo(p(6.0, 6.0), p(8.0, 4.0), p(8.0, 0.0)),
        ]
    );
    // Deleting the inserted anchor keeps the outer handles: (0,4) and (8,4).
    assert_eq!(
        delete_anchor(&halves, at(0, 1)).unwrap().elements,
        vec![E::MoveTo(p(0.0, 0.0)), E::CubicTo(p(0.0, 4.0), p(8.0, 4.0), p(8.0, 0.0))]
    );
    // Quadratic split: (0,0) (4,8) (8,0) at 0.5 -> (2,4) (4,4) | (6,4) (8,0).
    let quad = path(vec![E::MoveTo(p(0.0, 0.0)), E::QuadTo(p(4.0, 8.0), p(8.0, 0.0))]);
    assert_eq!(
        insert_anchor(&quad, 0, 0, 0.5).unwrap().elements[1..],
        [E::QuadTo(p(2.0, 4.0), p(4.0, 4.0)), E::QuadTo(p(6.0, 4.0), p(8.0, 0.0))]
    );
    // Closed square: inserting on the implicit closing segment, deleting a corner, deleting anchor 0.
    let sq = square();
    let with_mid = insert_anchor(&sq, 0, 3, 0.5).unwrap();
    assert_eq!(with_mid.elements[4], E::LineTo(p(0.0, 5.0)));
    let triangle = delete_anchor(&sq, at(0, 2)).unwrap();
    assert_eq!(triangle.elements, vec![E::MoveTo(p(0.0, 0.0)), E::LineTo(p(10.0, 0.0)), E::LineTo(p(0.0, 10.0)), E::Close]);
    let rotated = delete_anchor(&sq, at(0, 0)).unwrap();
    assert_eq!(rotated.elements, vec![E::MoveTo(p(0.0, 10.0)), E::LineTo(p(10.0, 0.0)), E::LineTo(p(10.0, 10.0)), E::Close]);
    // Open ends drop their segment.
    let three = path(vec![E::MoveTo(p(0.0, 0.0)), E::LineTo(p(1.0, 0.0)), E::LineTo(p(2.0, 0.0))]);
    assert_eq!(delete_anchor(&three, at(0, 0)).unwrap().elements, vec![E::MoveTo(p(1.0, 0.0)), E::LineTo(p(2.0, 0.0))]);
    assert_eq!(delete_anchor(&three, at(0, 2)).unwrap().elements, vec![E::MoveTo(p(0.0, 0.0)), E::LineTo(p(1.0, 0.0))]);
}

#[test]
fn convert_scissors_join_average() {
    // Smooth the square's corner (10,0): neighbours (0,0) and (10,10); chord direction (1,1)/sqrt2,
    // handle lengths 10/3 each.
    let smooth = convert_anchor(&square(), at(0, 1), AnchorKind::Smooth).unwrap();
    let h = 10.0 / 3.0 / 2f64.sqrt();
    assert_eq!(smooth.elements[1], E::CubicTo(p(0.0, 0.0), p(10.0 - h, -h), p(10.0, 0.0)));
    assert_eq!(smooth.elements[2], E::CubicTo(p(10.0 + h, h), p(10.0, 10.0), p(10.0, 10.0)));
    // Back to a corner: handles retract and both segments are lines again.
    assert_eq!(convert_anchor(&smooth, at(0, 1), AnchorKind::Corner).unwrap(), square());
    // Scissors at an interior anchor of an open path: two subpaths sharing the point.
    let open = path(vec![E::MoveTo(p(0.0, 0.0)), E::LineTo(p(5.0, 0.0)), E::LineTo(p(5.0, 5.0))]);
    let two = split_at_anchor(&open, at(0, 1)).unwrap();
    assert_eq!(two.elements, vec![E::MoveTo(p(0.0, 0.0)), E::LineTo(p(5.0, 0.0)), E::MoveTo(p(5.0, 0.0)), E::LineTo(p(5.0, 5.0))]);
    assert_eq!(split_at_anchor(&open, at(0, 0)).err(), Some(Error::InvalidRequest));
    // Scissors on a closed square opens it at (10,10): start and end there.
    let opened = split_at_anchor(&square(), at(0, 2)).unwrap();
    assert_eq!(
        opened.elements,
        vec![E::MoveTo(p(10.0, 10.0)), E::LineTo(p(0.0, 10.0)), E::LineTo(p(0.0, 0.0)), E::LineTo(p(10.0, 0.0)), E::LineTo(p(10.0, 10.0))]
    );
    // Scissors at a parametric point (STU-VEC-048): the segment midpoint becomes both new ends.
    let cut = split_at(&open, 0, 0, 0.5).unwrap();
    assert_eq!(cut.elements[..2], [E::MoveTo(p(0.0, 0.0)), E::LineTo(p(2.5, 0.0))]);
    assert_eq!(cut.elements[2], E::MoveTo(p(2.5, 0.0)));
    // Join: re-joining the two halves at coincident ends restores the original.
    let rejoined = join(&two, EndRef { subpath: 0, end: End::End }, EndRef { subpath: 1, end: End::Start }, 0.0).unwrap();
    assert_eq!(rejoined, open);
    // Non-coincident ends are connected by a line; start/end orientation is honoured.
    let pair = path(vec![E::MoveTo(p(0.0, 0.0)), E::LineTo(p(1.0, 0.0)), E::MoveTo(p(3.0, 0.0)), E::LineTo(p(2.0, 0.0))]);
    let joined = join(&pair, EndRef { subpath: 0, end: End::End }, EndRef { subpath: 1, end: End::End }, 0.0).unwrap();
    assert_eq!(joined.elements, vec![E::MoveTo(p(0.0, 0.0)), E::LineTo(p(1.0, 0.0)), E::LineTo(p(2.0, 0.0)), E::LineTo(p(3.0, 0.0))]);
    // Joining a subpath's own ends closes it.
    let closed = join(&opened, EndRef { subpath: 0, end: End::End }, EndRef { subpath: 0, end: End::Start }, 0.0).unwrap();
    assert!(closed.is_closed());
    assert_eq!(closed.elements.len(), 5);
    // Average two anchors on both axes and on one axis.
    let avg = average_anchors(&open, &[at(0, 0), at(0, 2)], Axis::Both).unwrap();
    assert_eq!(avg.elements, vec![E::MoveTo(p(2.5, 2.5)), E::LineTo(p(5.0, 0.0)), E::LineTo(p(2.5, 2.5))]);
    let vertical = average_anchors(&open, &[at(0, 0), at(0, 2)], Axis::Vertical).unwrap();
    assert_eq!(vertical.elements[0], E::MoveTo(p(2.5, 0.0)));
    assert_eq!(vertical.elements[2], E::LineTo(p(2.5, 5.0)));
}

#[test]
fn offset_path_hand_areas() {
    let opts = BooleanOptions::default();
    // Outset 1 with miter corners: 12 x 12; bevel cuts four half unit squares; round adds quarter discs.
    assert_eq!(area(&offset_path(&square(), 1.0, LineJoin::Miter, 4.0, &opts).unwrap().path), 144.0);
    assert_eq!(area(&offset_path(&square(), 1.0, LineJoin::Bevel, 4.0, &opts).unwrap().path), 142.0);
    let round = area(&offset_path(&square(), 1.0, LineJoin::Round, 4.0, &opts).unwrap().path);
    assert!(round < 140.0 + std::f64::consts::PI && round > 140.0 + std::f64::consts::PI - 0.2, "{round}");
    // Inset 1: 8 x 8; inset past the middle is empty (typed).
    assert_eq!(area(&offset_path(&square(), -1.0, LineJoin::Miter, 4.0, &opts).unwrap().path), 64.0);
    assert_eq!(offset_path(&square(), -6.0, LineJoin::Miter, 4.0, &opts).err(), Some(Error::EmptyPathfinderResult));
    // Zero offset returns the resolved region; a frame (square with hole) shrinks its hole on outset.
    assert_eq!(area(&offset_path(&square(), 0.0, LineJoin::Miter, 4.0, &opts).unwrap().path), 100.0);
    let mut frame = square();
    frame.elements.extend([E::MoveTo(p(3.0, 3.0)), E::LineTo(p(3.0, 7.0)), E::LineTo(p(7.0, 7.0)), E::LineTo(p(7.0, 3.0)), E::Close]);
    assert_eq!(area(&offset_path(&frame, 0.0, LineJoin::Miter, 4.0, &opts).unwrap().path), 84.0);
    assert_eq!(area(&offset_path(&frame, 1.0, LineJoin::Miter, 4.0, &opts).unwrap().path), 144.0 - 4.0);
}

#[test]
fn edit_hostile_inputs_are_typed() {
    let sq = square();
    for t in [0.0, 1.0, -0.5, f64::NAN] {
        assert_eq!(insert_anchor(&sq, 0, 0, t).err(), Some(Error::InvalidRequest), "{t}");
    }
    assert_eq!(insert_anchor(&sq, 1, 0, 0.5).err(), Some(Error::InvalidRequest));
    assert_eq!(insert_anchor(&sq, 0, 4, 0.5).err(), Some(Error::InvalidRequest));
    assert_eq!(delete_anchor(&sq, at(0, 4)).err(), Some(Error::InvalidRequest));
    let line = path(vec![E::MoveTo(p(0.0, 0.0)), E::LineTo(p(1.0, 0.0))]);
    assert_eq!(delete_anchor(&line, at(0, 0)).err(), Some(Error::InvalidGeometry));
    assert_eq!(convert_anchor(&sq, at(0, 9), AnchorKind::Corner).err(), Some(Error::InvalidRequest));
    let dot = path(vec![E::MoveTo(p(0.0, 0.0)), E::LineTo(p(0.0, 0.0))]);
    assert_eq!(convert_anchor(&dot, at(0, 0), AnchorKind::Smooth).err(), Some(Error::InvalidGeometry));
    let mut nan = sq.clone();
    nan.elements[1] = E::LineTo(p(f64::NAN, 0.0));
    assert_eq!(delete_anchor(&nan, at(0, 1)).err(), Some(Error::InvalidGeometry));
    assert_eq!(join(&sq, EndRef { subpath: 0, end: End::End }, EndRef { subpath: 0, end: End::Start }, 0.0).err(), Some(Error::InvalidRequest));
    assert_eq!(join(&line, EndRef { subpath: 0, end: End::End }, EndRef { subpath: 0, end: End::Start }, f64::NAN).err(), Some(Error::InvalidRequest));
    assert_eq!(average_anchors(&sq, &[], Axis::Both).err(), Some(Error::InvalidRequest));
    assert_eq!(average_anchors(&sq, &[at(3, 0)], Axis::Both).err(), Some(Error::InvalidRequest));
    let opts = BooleanOptions::default();
    for bad in [f64::NAN, f64::INFINITY, 2e9] {
        assert_eq!(offset_path(&sq, bad, LineJoin::Miter, 4.0, &opts).err(), Some(Error::UnsupportedOptions));
    }
    assert_eq!(offset_path(&sq, 1.0, LineJoin::Miter, 0.5, &opts).err(), Some(Error::UnsupportedOptions));
    // Inputs are never mutated: the source square is unchanged after every call above.
    assert_eq!(sq, square());
}
