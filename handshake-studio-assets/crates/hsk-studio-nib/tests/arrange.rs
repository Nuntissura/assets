//! Lane SMB-ILLUSTRATOR-TOOLS-2: transforms about reference points, transform-each, align and
//! distribute (STU-VEC-157, STU-VEC-166). Expected values are hand-derived.
use hsk_studio_nib::arrange::*;
use hsk_studio_nib::{Error, FillRule, OwnedPath, PathElement, PathPoint};

fn b(x0: f64, y0: f64, x1: f64, y1: f64) -> Bounds {
    Bounds { min: [x0, y0], max: [x1, y1] }
}
fn apply(m: &[f64; 6], x: f64, y: f64) -> [f64; 2] {
    [m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]]
}
fn near(a: [f64; 2], e: [f64; 2]) {
    assert!((a[0] - e[0]).abs() < 1e-9 && (a[1] - e[1]).abs() < 1e-9, "{a:?} != {e:?}");
}

#[test]
fn transforms_transform_each_align_and_distribute() {
    // Exact bounds of a cubic (extremum at t = 0.5: 0.75 * 10), not the control hull.
    let mut c = OwnedPath::new(FillRule::NonZero);
    c.elements = vec![PathElement::MoveTo(PathPoint::new(0.0, 0.0)), PathElement::CubicTo(PathPoint::new(0.0, 10.0), PathPoint::new(10.0, 10.0), PathPoint::new(10.0, 0.0))];
    assert_eq!(path_bounds(&c), Some(b(0.0, 0.0, 10.0, 7.5)));
    // Rotate (clockwise on screen), scale, reflect, shear about a point.
    near(apply(&rotate_about([10.0, 10.0], 90.0).unwrap(), 20.0, 10.0), [10.0, 20.0]);
    near(apply(&scale_about([10.0, 10.0], 2.0, 3.0).unwrap(), 11.0, 11.0), [12.0, 13.0]);
    near(apply(&reflect_about([0.0, 5.0], 0.0).unwrap(), 3.0, 7.0), [3.0, 3.0]);
    near(apply(&reflect_about([5.0, 0.0], 90.0).unwrap(), 7.0, 3.0), [3.0, 3.0]);
    near(apply(&shear_about([0.0, 0.0], 45.0, 0.0).unwrap(), 2.0, 3.0), [5.0, 3.0]);
    // Transform-each: every object about its own centre, then moved.
    let boxes = [b(0.0, 0.0, 10.0, 10.0), b(100.0, 0.0, 120.0, 20.0)];
    let p = TransformEach { scale_horizontal: 200.0, scale_vertical: 50.0, move_horizontal: 5.0, ..TransformEach::default() };
    let m = transform_each(&boxes, &p).unwrap();
    near(apply(&m[0], 10.0, 10.0), [20.0, 7.5]);
    near(apply(&m[1], 100.0, 0.0), [95.0, 5.0]);
    let tl = TransformEach { rotate: 90.0, reference: ReferencePoint::from_wire(1).unwrap(), ..TransformEach::default() };
    near(apply(&transform_each(&boxes, &tl).unwrap()[1], 110.0, 0.0), [100.0, 10.0]);
    let random = TransformEach { rotate: 90.0, randomize: true, seed: 7, ..TransformEach::default() };
    assert_eq!(transform_each(&boxes, &random).unwrap(), transform_each(&boxes, &random).unwrap(), "seeded");
    // Align: left edges to the selection; right edges to a key object (which stays put).
    let row = [b(0.0, 0.0, 10.0, 10.0), b(20.0, 5.0, 40.0, 15.0), b(50.0, 0.0, 55.0, 30.0)];
    assert_eq!(align(&row, AlignTo::Selection, Align::Left).unwrap(), vec![[0.0, 0.0], [-20.0, 0.0], [-50.0, 0.0]]);
    assert_eq!(align(&row, AlignTo::KeyObject(1), Align::Right).unwrap(), vec![[30.0, 0.0], [0.0, 0.0], [-15.0, 0.0]]);
    assert_eq!(align(&row, AlignTo::Artboard(b(0.0, 0.0, 100.0, 100.0)), Align::Bottom).unwrap(), vec![[0.0, 90.0], [0.0, 85.0], [0.0, 70.0]]);
    // Distribute centres: 5, 30, 52.5 -> 5, 28.75, 52.5.
    assert_eq!(distribute(&row, AlignTo::Selection, Distribute::Edge(Align::CentreHorizontal)).unwrap(), vec![[0.0, 0.0], [-1.25, 0.0], [0.0, 0.0]]);
    // Equal spacing: total width 35 inside 0..55 -> gaps 10.
    assert_eq!(distribute(&row, AlignTo::Selection, Distribute::Spacing { axis: 0, gap: None }).unwrap(), vec![[0.0, 0.0]; 3]);
    // Inside an artboard 0..100 the extremes touch its edges: gaps (100 - 35) / 2 = 32.5.
    assert_eq!(distribute(&row, AlignTo::Artboard(b(0.0, 0.0, 100.0, 100.0)), Distribute::Spacing { axis: 0, gap: None }).unwrap(), vec![[0.0, 0.0], [22.5, 0.0], [45.0, 0.0]]);
    // Explicit 4 px gaps packed from the key object (index 1): left neighbour ends at 16, right starts at 44.
    assert_eq!(distribute(&row, AlignTo::KeyObject(1), Distribute::Spacing { axis: 0, gap: Some(4.0) }).unwrap(), vec![[6.0, 0.0], [0.0, 0.0], [-6.0, 0.0]]);
    // Hostile input is refused before anything is computed.
    assert_eq!(rotate_about([f64::NAN, 0.0], 1.0), Err(Error::InvalidRequest));
    assert_eq!(transform_each(&boxes, &TransformEach { scale_horizontal: 5000.0, ..TransformEach::default() }), Err(Error::InvalidRequest));
    assert_eq!(transform_each(&vec![b(0.0, 0.0, 1.0, 1.0); MAX_ARRANGE_OBJECTS + 1], &TransformEach::default()), Err(Error::BudgetExceeded));
    assert_eq!(align(&row, AlignTo::KeyObject(9), Align::Left), Err(Error::InvalidRequest));
    assert_eq!(shear_about([0.0, 0.0], 90.0, 0.0), Err(Error::InvalidRequest));
}
