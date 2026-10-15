//! Lane SMB-AE-MOTION-TOOLS slice 3: shape-layer path operators (STU-MOT-101/103/104). Expected
//! values are hand-computed in the comments (After Effects parity UNVERIFIED, D5).
use hsk_studio_nib::boolean::BooleanOptions;
use hsk_studio_nib::shape_ops::*;
use hsk_studio_nib::stroke::{LineJoin, StrokeOptions};
use hsk_studio_nib::{FillRule, OwnedPath, PathElement, PathPoint};

fn pt(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}

fn poly(points: &[(f64, f64)], closed: bool) -> OwnedPath {
    let mut path = OwnedPath::new(FillRule::NonZero);
    for (i, (x, y)) in points.iter().enumerate() {
        path.elements.push(if i == 0 { PathElement::MoveTo(pt(*x, *y)) } else { PathElement::LineTo(pt(*x, *y)) });
    }
    if closed {
        path.elements.push(PathElement::Close);
    }
    path
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> OwnedPath {
    poly(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)], true)
}

fn lines(path: &OwnedPath) -> Vec<(f64, f64)> {
    path.elements
        .iter()
        .filter_map(|e| match e {
            PathElement::MoveTo(p) | PathElement::LineTo(p) => Some((p.x, p.y)),
            _ => None,
        })
        .collect()
}

/// Net signed shoelace area over the polygon subpaths of a boolean / offset result.
fn area(path: &OwnedPath) -> f64 {
    let (mut total, mut ring): (f64, Vec<PathPoint>) = (0.0, Vec::new());
    let close = |ring: &mut Vec<PathPoint>| {
        let n = ring.len();
        let mut a = 0.0;
        for i in 0..n {
            let (p, q) = (ring[i], ring[(i + 1) % n]);
            a += p.x * q.y - q.x * p.y;
        }
        ring.clear();
        a / 2.0
    };
    for e in &path.elements {
        match e {
            PathElement::MoveTo(p) => {
                if !ring.is_empty() {
                    total += close(&mut ring);
                }
                ring.push(*p);
            }
            PathElement::LineTo(p) => ring.push(*p),
            PathElement::Close => total += close(&mut ring),
            other => panic!("polygon expected, got {other:?}"),
        }
    }
    if !ring.is_empty() {
        total += close(&mut ring);
    }
    total.abs()
}

fn near(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol, "{a} != {b} (tol {tol})");
}

#[test]
fn trim_repeater_offset_and_merge_match_hand_values() {
    // 100 x 50 rectangle, perimeter 300, starting at (0, 0) clockwise on the y-down canvas.
    let r = rect(0.0, 0.0, 100.0, 50.0);
    near(path_length(&r).unwrap(), 300.0, 1e-9);
    let half = trim_path(&r, 0.0, 50.0, 0.0, TrimMode::Simultaneously).unwrap();
    assert_eq!(lines(&half), vec![(0.0, 0.0), (100.0, 0.0), (100.0, 50.0)]);
    near(path_length(&half).unwrap(), 150.0, 1e-9);
    // Offset 90 deg = a quarter of the length (75): [75, 225].
    let shifted = trim_path(&r, 0.0, 50.0, 90.0, TrimMode::Simultaneously).unwrap();
    assert_eq!(lines(&shifted), vec![(75.0, 0.0), (100.0, 0.0), (100.0, 50.0), (25.0, 50.0)]);
    // Offset 270 deg: [225, 300] + [0, 75] wraps into ONE continuous contour through the start.
    let wrapped = trim_path(&r, 0.0, 50.0, 270.0, TrimMode::Simultaneously).unwrap();
    assert_eq!(lines(&wrapped), vec![(25.0, 50.0), (0.0, 50.0), (0.0, 0.0), (75.0, 0.0)]);
    assert!(trim_path(&r, 30.0, 30.0, 0.0, TrimMode::Simultaneously).unwrap().elements.is_empty());
    assert_eq!(trim_path(&r, 0.0, 100.0, 45.0, TrimMode::Simultaneously).unwrap(), r, "a full range keeps the path");
    // Individually: lengths 100 + 300 = 400, keep [0, 200] => A whole, B its first 100.
    let (a, b) = (poly(&[(0.0, 0.0), (100.0, 0.0)], false), poly(&[(0.0, 10.0), (300.0, 10.0)], false));
    let ind = trim_many(&[a.clone(), b.clone()], 0.0, 50.0, 0.0, TrimMode::Individually).unwrap();
    assert_eq!((lines(&ind[0]), lines(&ind[1])), (vec![(0.0, 0.0), (100.0, 0.0)], vec![(0.0, 10.0), (100.0, 10.0)]));
    let sim = trim_many(&[a, b], 0.0, 50.0, 0.0, TrimMode::Simultaneously).unwrap();
    assert_eq!((lines(&sim[0]), lines(&sim[1])), (vec![(0.0, 0.0), (50.0, 0.0)], vec![(0.0, 10.0), (150.0, 10.0)]));
    // Quarter-circle cubic (circle constant): length ~ 50 pi; its arc-length midpoint is t = 0.5 by
    // symmetry: 0.125 * 100 + 0.375 * 100 + 0.375 * 55.228... = 70.710678...
    let k = 100.0 * 4.0 * (2f64.sqrt() - 1.0) / 3.0;
    let mut arc = OwnedPath::new(FillRule::NonZero);
    arc.elements = vec![PathElement::MoveTo(pt(100.0, 0.0)), PathElement::CubicTo(pt(100.0, k), pt(k, 100.0), pt(0.0, 100.0))];
    near(path_length(&arc).unwrap(), 50.0 * std::f64::consts::PI, 0.05);
    let first_half = trim_path(&arc, 0.0, 50.0, 0.0, TrimMode::Simultaneously).unwrap();
    let PathElement::CubicTo(_, _, end) = first_half.elements[1] else { panic!("cubic piece") };
    near(end.x, 12.5 + 37.5 + 0.375 * k, 1e-6);
    near(end.y, end.x, 1e-6);

    // Repeater: position (10, 0) x 3 => translations 0, 10, 20; opacity ramp 100 -> 40.
    let mut t = RepeaterTransform { anchor: [0.0, 0.0], position: [10.0, 0.0], scale: [100.0, 100.0], rotation: 0.0, start_opacity: 100.0, end_opacity: 40.0 };
    let copies = repeater(3.0, 0.0, &t).unwrap();
    assert_eq!(copies.iter().map(|(m, _)| m[4]).collect::<Vec<_>>(), vec![0.0, 10.0, 20.0]);
    let op: Vec<f64> = copies.iter().map(|c| c.1).collect();
    near(op[1], 0.7, 1e-12);
    near(op[2], 0.4, 1e-12);
    // Cumulative: rotation 90 + position (10, 0): T(origin) = (10, 0), T^2(origin) = R(10, 0) + (10, 0) = (10, 10).
    t.rotation = 90.0;
    let walk = repeater(3.0, 0.0, &t).unwrap();
    assert_eq!((walk[2].0[4], walk[2].0[5]), (10.0, 10.0));
    // Negative offset = the inverse step: T^-1(origin) = R^-1((0, 0) - (10, 0)) = (0, 10).
    let back = repeater(1.0, -1.0, &t).unwrap();
    near(back[0].0[4], 0.0, 1e-12);
    near(back[0].0[5], 10.0, 1e-12);
    // Fractional copies: 2.5 => three copies, the last at half opacity.
    t.end_opacity = 100.0;
    assert_eq!(repeater(2.5, 0.0, &t).unwrap().iter().map(|c| c.1).collect::<Vec<_>>(), vec![1.0, 1.0, 0.5]);

    // Offset paths, miter join: +10 => 120 x 70 = 8400, -10 => 80 x 30 = 2400.
    let opts = StrokeOptions::default();
    near(area(&offset_path(&r, 10.0, LineJoin::Miter, 4.0, 1, &opts).unwrap()), 8400.0, 1e-3);
    near(area(&offset_path(&r, -10.0, LineJoin::Miter, 4.0, 1, &opts).unwrap()), 2400.0, 1e-3);

    // Merge: A [0,100]^2, B [50,150] x [0,100].
    let (ma, mb) = (rect(0.0, 0.0, 100.0, 100.0), rect(50.0, 0.0, 150.0, 100.0));
    let bo = BooleanOptions::default();
    for (mode, want) in [(MergeMode::Add, 15000.0), (MergeMode::Subtract, 5000.0), (MergeMode::Intersect, 5000.0), (MergeMode::ExcludeIntersections, 10000.0)] {
        near(area(&merge_paths(&[ma.clone(), mb.clone()], mode, &bo).unwrap()), want, 1e-3);
    }
    assert_eq!(merge_paths(&[ma.clone(), mb.clone()], MergeMode::Merge, &bo).unwrap().elements.len(), 10);
}

#[test]
fn point_modifiers_generators_and_hostile_inputs() {
    // Bloat 50 % on a square centred at the origin: vertices halfway to the centre, the straight
    // edges' control points (= their end points) pushed out by half: the top edge becomes
    // Cubic((-25,-25), (-75,-75), (75,-75), (25,-25)), whose midpoint y is
    // 0.125 * -25 + 0.375 * -75 * 2 + 0.125 * -25 = -62.5.
    let sq = rect(-50.0, -50.0, 50.0, 50.0);
    let b = pucker_bloat(&sq, 50.0).unwrap();
    assert_eq!(b.elements[0], PathElement::MoveTo(pt(-25.0, -25.0)));
    assert_eq!(b.elements[1], PathElement::CubicTo(pt(-75.0, -75.0), pt(75.0, -75.0), pt(25.0, -25.0)));

    // Round corners radius 10 on [0,100]^2: the corner (100, 0) becomes the arc (90,0) -> (100,10)
    // with handle 4/3 tan(22.5 deg) * 10; the path starts at the cut point (10, 0).
    let h = 4.0 / 3.0 * (2f64.sqrt() - 1.0) * 10.0;
    let rounded = round_corners(&rect(0.0, 0.0, 100.0, 100.0), 10.0).unwrap();
    assert_eq!(rounded.elements[0], PathElement::MoveTo(pt(10.0, 0.0)));
    let corner = rounded.elements.iter().find_map(|e| match e {
        PathElement::CubicTo(a, b, c) if *c == pt(100.0, 10.0) => Some((*a, *b)),
        _ => None,
    });
    let (c1, c2) = corner.expect("corner arc");
    near(c1.x, 90.0 + h, 1e-12);
    near(c2.y, 10.0 - h, 1e-12);

    // Zig zag, 3 ridges, size 10 on (0,0)-(100,0): normal (0, 1), signs + - +.
    let line = poly(&[(0.0, 0.0), (100.0, 0.0)], false);
    let z = zig_zag(&line, 10.0, 3, false).unwrap();
    assert_eq!(lines(&z), vec![(0.0, 0.0), (25.0, 10.0), (50.0, -10.0), (75.0, 10.0), (100.0, 0.0)]);

    // Wiggle: size 0 = the subdivided points; size 5 stays within 5 per axis; seeded.
    let w = WiggleParams { size: 0.0, detail: 1, smooth: false, time_s: 0.4, wiggles_per_second: 3.0, correlation: 25.0, temporal_phase: 0.0, spatial_phase: 30.0, seed: 9 };
    assert_eq!(lines(&wiggle_path(&line, &w).unwrap()), vec![(0.0, 0.0), (50.0, 0.0), (100.0, 0.0)]);
    let moved = wiggle_path(&line, &WiggleParams { size: 5.0, ..w }).unwrap();
    for ((x, y), (bx, by)) in lines(&moved).into_iter().zip([(0.0, 0.0), (50.0, 0.0), (100.0, 0.0)]) {
        assert!((x - bx).abs() <= 5.0 && (y - by).abs() <= 5.0);
    }
    assert_eq!(moved, wiggle_path(&line, &WiggleParams { size: 5.0, ..w }).unwrap());
    assert_ne!(moved, wiggle_path(&line, &WiggleParams { size: 5.0, seed: 10, ..w }).unwrap());

    // Twist 90 about the origin, R = 100: (50, 0) at half radius rotates 45 deg; ends stay.
    let tw = twist(&poly(&[(0.0, 0.0), (50.0, 0.0), (100.0, 0.0)], false), 90.0, [0.0, 0.0]).unwrap();
    let PathElement::CubicTo(_, _, mid) = tw.elements[1] else { panic!("cubic") };
    near(mid.x, 50.0 / 2f64.sqrt(), 1e-9);
    near(mid.y, 50.0 / 2f64.sqrt(), 1e-9);
    let PathElement::CubicTo(_, _, end) = tw.elements[2] else { panic!("cubic") };
    assert_eq!(end, pt(100.0, 0.0));

    // Polystar: 5-point star, outer 100, inner 50: first vertex straight up, the second at -54 deg.
    let star = polystar_path(&Polystar { star: true, points: 5.0, position: [0.0, 0.0], rotation: 0.0, outer_radius: 100.0, inner_radius: 50.0, outer_roundness: 0.0, inner_roundness: 0.0, reversed: false }).unwrap();
    let v = lines(&star);
    assert_eq!(v.len(), 10);
    near(v[0].0, 0.0, 1e-12);
    near(v[0].1, -100.0, 1e-12);
    near(v[1].0, 50.0 * 54f64.to_radians().cos(), 1e-9);
    near(v[1].1, -50.0 * 54f64.to_radians().sin(), 1e-9);
    let rc = rect_path([50.0, 25.0], [100.0, 50.0], 0.0, false).unwrap();
    near(path_length(&rc).unwrap(), 300.0, 1e-9);
    let reversed = rect_path([50.0, 25.0], [100.0, 50.0], 0.0, true).unwrap();
    near(path_length(&reversed).unwrap(), 300.0, 1e-9);
    assert_ne!(lines(&reversed), lines(&rc));

    // Hostile: counts refused before allocation, non-finite parameters refused.
    let t = RepeaterTransform { anchor: [0.0; 2], position: [1.0, 0.0], scale: [100.0; 2], rotation: 0.0, start_opacity: 100.0, end_opacity: 100.0 };
    assert!(repeater(1025.0, 0.0, &t).is_err());
    assert!(repeater(f64::NAN, 0.0, &t).is_err());
    assert!(repeater(3.0, 0.0, &RepeaterTransform { scale: [0.0, 100.0], ..t }).is_err());
    assert!(zig_zag(&line, 1.0, MAX_POINTS_PER_SEGMENT + 1, false).is_err());
    let many: Vec<(f64, f64)> = (0..300).map(|i| (f64::from(i), 0.0)).collect();
    assert!(zig_zag(&poly(&many, false), 1.0, MAX_POINTS_PER_SEGMENT, false).is_err(), "aggregate point budget");
    assert!(trim_path(&line, f64::NAN, 50.0, 0.0, TrimMode::Simultaneously).is_err());
    assert!(polystar_path(&Polystar { star: false, points: 2.0, position: [0.0; 2], rotation: 0.0, outer_radius: 1.0, inner_radius: 1.0, outer_roundness: 0.0, inner_roundness: 0.0, reversed: false }).is_err());
    assert!(offset_path(&sq, 1.0, LineJoin::Miter, 4.0, 0, &StrokeOptions::default()).is_err());
    assert!(offset_path(&sq, 1.0, LineJoin::Miter, 4.0, MAX_OFFSET_COPIES + 1, &StrokeOptions::default()).is_err());
    assert!(wiggle_path(&line, &WiggleParams { detail: MAX_POINTS_PER_SEGMENT + 1, ..w }).is_err());
    assert!(round_corners(&sq, f64::INFINITY).is_err());
}
