//! Lane SMB-ILLUSTRATOR-PEN slice 3: the unified pathfinder (divide, trim, merge, crop and the five
//! single-result operations) and offset path / outline stroke on curves, against hand areas.
use hsk_studio_nib::anchor_path::{AnchorPath, Contour, EditAnchor, point_at};
use hsk_studio_nib::boolean::BooleanOptions;
use hsk_studio_nib::edit::offset_path;
use hsk_studio_nib::flatten::FlattenOptions;
use hsk_studio_nib::pathfinder::{PathfinderOp, pathfinder};
use hsk_studio_nib::shape::{EllipseSpec, ellipse, rectangle, RectSpec};
use hsk_studio_nib::stroke::{LineJoin, StrokeOptions, StrokeStyle, outline_stroke};
use hsk_studio_nib::{Error, FillRule, OwnedPath, PathElement as E, PathPoint};
use std::f64::consts::PI;

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

fn square(x: f64, y: f64, s: f64) -> OwnedPath {
    rectangle(&RectSpec::plain(x, y, s, s)).unwrap().0.to_owned_path().unwrap()
}

fn circle(cx: f64, cy: f64, r: f64) -> OwnedPath {
    ellipse(&EllipseSpec::full(cx, cy, r, r)).unwrap().to_owned_path().unwrap()
}

fn fine() -> BooleanOptions {
    BooleanOptions { flatten: FlattenOptions { tolerance: 0.001, ..BooleanOptions::default().flatten }, ..BooleanOptions::default() }
}

fn close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * b.abs().max(1.0)
}

#[test]
fn divide_squares_and_circles_into_faces() {
    let opts = BooleanOptions::default();
    let r = pathfinder(PathfinderOp::Divide, &[square(0.0, 0.0, 10.0), square(5.0, 5.0, 10.0)], None, &opts).unwrap();
    let got: Vec<(f64, Vec<usize>)> = r.pieces.iter().map(|p| (area(&p.path).abs(), p.sources.clone())).collect();
    assert_eq!(got, vec![(25.0, vec![0, 1]), (75.0, vec![0]), (75.0, vec![1])]);
    // Three disjoint-coverage faces of the same total area as the union.
    let union = pathfinder(PathfinderOp::Union, &[square(0.0, 0.0, 10.0), square(5.0, 5.0, 10.0)], None, &opts).unwrap();
    assert_eq!(area(&union.pieces[0].path), 175.0);
    assert_eq!(union.pieces[0].sources, vec![0, 1]);
    // Curves: two unit-10 circles 10 apart; lens = 2 r^2 acos(d / 2r) - (d / 2) sqrt(4 r^2 - d^2).
    let r10 = pathfinder(PathfinderOp::Divide, &[circle(0.0, 0.0, 10.0), circle(10.0, 0.0, 10.0)], None, &fine()).unwrap();
    let lens = 200.0 * (0.5_f64).acos() - 5.0 * 300.0_f64.sqrt();
    let disc = PI * 100.0;
    let areas: Vec<f64> = r10.pieces.iter().map(|p| area(&p.path).abs()).collect();
    assert_eq!(r10.pieces.len(), 3, "{areas:?}");
    assert!(close(areas[0], lens, 2e-3) && close(areas[1], disc - lens, 2e-3) && close(areas[2], disc - lens, 2e-3), "{areas:?} lens {lens}");
    // A ring divided by a bar crossing it: the bar splits the annulus into separate closed regions.
    let ring = ellipse(&EllipseSpec { inner_ratio: 0.5, ..EllipseSpec::full(0.0, 0.0, 10.0, 10.0) }).unwrap().to_owned_path().unwrap();
    let bar = rectangle(&RectSpec::plain(-12.0, -1.0, 24.0, 2.0)).unwrap().0.to_owned_path().unwrap();
    let cut = pathfinder(PathfinderOp::Divide, &[ring, bar], None, &fine()).unwrap();
    let ring_only = cut.pieces.iter().filter(|p| p.sources == vec![0]).count();
    assert_eq!(ring_only, 2, "upper and lower annulus halves are separate regions");
    // Every piece converts back to an editable anchor path.
    for p in &cut.pieces {
        AnchorPath::from_owned_path(&p.path).unwrap();
    }
}

#[test]
fn trim_merge_crop_hand_areas() {
    let opts = BooleanOptions::default();
    let ops = [square(0.0, 0.0, 10.0), square(5.0, 0.0, 10.0), square(30.0, 0.0, 10.0)];
    // Trim: the bottom square loses the overlap hidden by the middle one.
    let t = pathfinder(PathfinderOp::Trim, &ops, None, &opts).unwrap();
    let got: Vec<(f64, Vec<usize>)> = t.pieces.iter().map(|p| (area(&p.path).abs(), p.sources.clone())).collect();
    assert_eq!(got, vec![(50.0, vec![0]), (100.0, vec![1]), (100.0, vec![2])]);
    // Merge with keys [7, 7, 7]: the touching trimmed 0 and 1 merge (150); 2 does not touch, stays apart.
    let m = pathfinder(PathfinderOp::Merge, &ops, Some(&[7, 7, 7]), &opts).unwrap();
    let mut got: Vec<(f64, Vec<usize>)> = m.pieces.iter().map(|p| (area(&p.path).abs(), p.sources.clone())).collect();
    got.sort_by(|a, b| b.0.total_cmp(&a.0));
    assert_eq!(got, vec![(150.0, vec![0, 1]), (100.0, vec![2])]);
    // Different keys never merge: same pieces as trim.
    let m = pathfinder(PathfinderOp::Merge, &ops, Some(&[1, 2, 3]), &opts).unwrap();
    assert_eq!(m.pieces.len(), 3);
    // Crop: square 0 clipped to the topmost square (5,0)-(15,10), which is removed; with the topmost
    // = square 2 nothing below overlaps it: typed empty result.
    let c = pathfinder(PathfinderOp::Crop, &ops[..2], None, &opts).unwrap();
    assert_eq!(c.pieces.len(), 1);
    assert_eq!((area(&c.pieces[0].path).abs(), c.pieces[0].sources.clone()), (50.0, vec![0]));
    assert_eq!(pathfinder(PathfinderOp::Crop, &ops, None, &opts).err(), Some(Error::EmptyPathfinderResult));
    // Single-result operations keep their hand values through the unified entry point.
    let sub = pathfinder(PathfinderOp::SubtractFront, &ops[..2], None, &opts).unwrap();
    assert_eq!(area(&sub.pieces[0].path), 50.0);
    let x = pathfinder(PathfinderOp::Exclude, &ops[..2], None, &opts).unwrap();
    assert_eq!(area(&x.pieces[0].path), 100.0);
}

#[test]
fn offset_and_outline_stroke_on_curves() {
    let opts = fine();
    // Offset a circle (four cubics) by +2 and -2: areas pi (r +- 2)^2.
    let c = circle(0.0, 0.0, 10.0);
    let out = area(&offset_path(&c, 2.0, LineJoin::Round, 4.0, &opts).unwrap().path);
    let inn = area(&offset_path(&c, -2.0, LineJoin::Round, 4.0, &opts).unwrap().path);
    assert!(close(out, PI * 144.0, 2e-3) && close(inn, PI * 64.0, 2e-3), "{out} {inn}");
    // Outline stroke of an open half-circle arc (two cubics) width 2, butt caps: area = arc length * 2.
    let arc = ellipse(&EllipseSpec { start_angle: 0.0, end_angle: 180.0, ..EllipseSpec::full(0.0, 0.0, 10.0, 10.0) }).unwrap();
    let open_arc = AnchorPath {
        contours: vec![Contour { anchors: arc.contours[0].anchors[..3].to_vec(), closed: false }],
        fill_rule: FillRule::NonZero,
    };
    let stroke = outline_stroke(&open_arc.to_owned_path().unwrap(), &StrokeStyle::new(2.0), &StrokeOptions { geometry: opts, ..StrokeOptions::default() }).unwrap();
    let a = area(&stroke.path);
    assert!(close(a, PI * 10.0 * 2.0, 3e-3), "{a}");
    // An S curve drawn with the pen (smooth anchor in the middle) strokes to length * width too; the
    // length is measured by 20000-sample chords of the anchor model's own evaluation.
    let mut a0 = EditAnchor::corner(0.0, 0.0);
    a0.outgoing = PathPoint::new(6.0, 0.0);
    let mut a2 = EditAnchor::corner(20.0, 10.0);
    a2.incoming = PathPoint::new(-6.0, 0.0);
    let s = AnchorPath {
        contours: vec![Contour { anchors: vec![a0, EditAnchor::smooth(10.0, 5.0, 4.0, 4.0), a2], closed: false }],
        fill_rule: FillRule::NonZero,
    };
    let mut length = 0.0;
    for seg in 0..2 {
        let mut prev = point_at(&s, 0, seg, 0.0).unwrap();
        for k in 1..=10000 {
            let q = point_at(&s, 0, seg, f64::from(k) / 10000.0).unwrap();
            length += ((q.x - prev.x).powi(2) + (q.y - prev.y).powi(2)).sqrt();
            prev = q;
        }
    }
    let stroke = outline_stroke(&s.to_owned_path().unwrap(), &StrokeStyle::new(1.0), &StrokeOptions { geometry: opts, ..StrokeOptions::default() }).unwrap();
    assert!(close(area(&stroke.path), length, 3e-3), "{} vs {length}", area(&stroke.path));
}

#[test]
fn tokens_and_hostile_inputs_are_typed() {
    let opts = BooleanOptions::default();
    // The thirteen STU-VEC-130 tokens parse to themselves; unknown text is refused.
    for op in PathfinderOp::ALL {
        assert_eq!(PathfinderOp::parse(op.wire()), Ok(op));
    }
    assert_eq!(PathfinderOp::parse("divide"), Err(Error::UnsupportedOperation));
    assert_eq!(PathfinderOp::Divide.member(), "geom.divide");
    let two = [square(0.0, 0.0, 10.0), square(5.0, 5.0, 10.0)];
    for op in [PathfinderOp::Outline, PathfinderOp::HardMix, PathfinderOp::SoftMix, PathfinderOp::Trap] {
        assert_eq!(pathfinder(op, &two, None, &opts).err(), Some(Error::UnsupportedOperation));
    }
    assert_eq!(pathfinder(PathfinderOp::Merge, &two, None, &opts).err(), Some(Error::InvalidRequest));
    assert_eq!(pathfinder(PathfinderOp::Merge, &two, Some(&[1]), &opts).err(), Some(Error::InvalidRequest));
    assert_eq!(pathfinder(PathfinderOp::Divide, &two[..1], None, &opts).err(), Some(Error::InvalidRequest));
    // NaN geometry, operand budget and the candidate-pair guard refuse before i_overlay runs.
    let nan = OwnedPath { elements: vec![E::MoveTo(PathPoint::new(f64::NAN, 0.0)), E::LineTo(PathPoint::new(1.0, 1.0)), E::Close], fill_rule: FillRule::NonZero };
    assert!(pathfinder(PathfinderOp::Divide, &[nan, two[0].clone()], None, &opts).is_err());
    let few = BooleanOptions { max_operands: 2, ..opts };
    assert_eq!(pathfinder(PathfinderOp::Trim, &[two[0].clone(), two[1].clone(), two[0].clone()], None, &few).err(), Some(Error::BudgetExceeded));
    let guard = BooleanOptions { max_candidate_pairs: 1, ..opts };
    assert_eq!(pathfinder(PathfinderOp::Divide, &[circle(0.0, 0.0, 10.0), circle(10.0, 0.0, 10.0)], None, &guard).err(), Some(Error::BudgetExceeded));
    // Degenerate operands (zero area) divide to nothing: typed empty result.
    let flat = rectangle(&RectSpec::plain(0.0, 0.0, 10.0, 0.0)).unwrap().0.to_owned_path().unwrap();
    assert_eq!(pathfinder(PathfinderOp::Divide, &[flat.clone(), flat], None, &opts).err(), Some(Error::EmptyPathfinderResult));
}
