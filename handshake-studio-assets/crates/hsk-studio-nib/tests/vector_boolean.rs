//! Lane SMB-NIB-VECTOR-RENDER slice 1: flattening bound and Boolean operations on arbitrary paths.
use hsk_studio_nib::boolean::{BooleanOp, BooleanOptions, boolean, normalise_path};
use hsk_studio_nib::flatten::{FlattenOptions, flatten};
use hsk_studio_nib::{Error, FillRule, OwnedPath, PathElement, PathPoint};

fn p(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}

fn polygon(points: &[(f64, f64)], rule: FillRule) -> OwnedPath {
    let mut path = OwnedPath::new(rule);
    for (i, &(x, y)) in points.iter().enumerate() {
        path.elements.push(if i == 0 {
            PathElement::MoveTo(p(x, y))
        } else {
            PathElement::LineTo(p(x, y))
        });
    }
    path.elements.push(PathElement::Close);
    path
}

fn square(x: f64, y: f64, s: f64) -> OwnedPath {
    polygon(&[(x, y), (x + s, y), (x + s, y + s), (x, y + s)], FillRule::NonZero)
}

/// Circle of four cubic arcs (kappa 0.5522847498).
fn circle(cx: f64, cy: f64, r: f64) -> OwnedPath {
    let k = 0.552_284_749_830_793_4 * r;
    let mut path = OwnedPath::new(FillRule::NonZero);
    path.elements.push(PathElement::MoveTo(p(cx + r, cy)));
    path.elements.push(PathElement::CubicTo(p(cx + r, cy + k), p(cx + k, cy + r), p(cx, cy + r)));
    path.elements.push(PathElement::CubicTo(p(cx - k, cy + r), p(cx - r, cy + k), p(cx - r, cy)));
    path.elements.push(PathElement::CubicTo(p(cx - r, cy - k), p(cx - k, cy - r), p(cx, cy - r)));
    path.elements.push(PathElement::CubicTo(p(cx + k, cy - r), p(cx + r, cy - k), p(cx + r, cy)));
    path.elements.push(PathElement::Close);
    path
}

/// Signed shoelace area per contour of a polygon path.
fn contour_areas(path: &OwnedPath) -> Vec<f64> {
    let mut out = Vec::new();
    let mut pts: Vec<PathPoint> = Vec::new();
    for e in &path.elements {
        match *e {
            PathElement::MoveTo(q) => pts = vec![q],
            PathElement::LineTo(q) => pts.push(q),
            PathElement::Close => {
                let n = pts.len();
                let mut a = 0.0;
                for i in 0..n {
                    let (u, v) = (pts[i], pts[(i + 1) % n]);
                    a += u.x * v.y - v.x * u.y;
                }
                out.push(a / 2.0);
            }
            _ => panic!("boolean output must be polygonal"),
        }
    }
    out
}

fn area(path: &OwnedPath) -> f64 {
    contour_areas(path).iter().sum()
}

fn options(tolerance: f64) -> BooleanOptions {
    let mut o = BooleanOptions::default();
    o.flatten.tolerance = tolerance;
    o
}

#[test]
fn flatten_error_within_tolerance_and_hostile_options() {
    // A strongly curved S cubic and a quad; dense samples must lie within tol of the polyline.
    let (a, b, c, d) = (p(0.0, 0.0), p(100.0, 300.0), p(200.0, -300.0), p(300.0, 0.0));
    let mut path = OwnedPath::new(FillRule::NonZero);
    path.elements.push(PathElement::MoveTo(a));
    path.elements.push(PathElement::CubicTo(b, c, d));
    path.elements.push(PathElement::QuadTo(p(400.0, 200.0), p(500.0, 0.0)));
    for tol in [1.0, 0.1, 0.01] {
        let opts = FlattenOptions { tolerance: tol, ..FlattenOptions::default() };
        let lines = flatten(&path, &opts).unwrap();
        assert_eq!(lines.len(), 1);
        let line = &lines[0];
        assert!(!line.closed);
        assert_eq!(line.points.first(), Some(&a));
        assert_eq!(line.points.last(), Some(&p(500.0, 0.0)));
        // Anchors are the three element end points only.
        assert_eq!(line.anchors.iter().filter(|f| **f).count(), 3);
        let dist = |q: PathPoint| {
            line.points
                .windows(2)
                .map(|w| seg_dist(q, w[0], w[1]))
                .fold(f64::INFINITY, f64::min)
        };
        let mut worst: f64 = 0.0;
        for i in 0..=4000 {
            let t = i as f64 / 4000.0;
            let s = 1.0 - t;
            let q = p(
                s * s * s * a.x + 3.0 * s * s * t * b.x + 3.0 * s * t * t * c.x + t * t * t * d.x,
                s * s * s * a.y + 3.0 * s * s * t * b.y + 3.0 * s * t * t * c.y + t * t * t * d.y,
            );
            worst = worst.max(dist(q));
            let q2 = p(
                s * s * 300.0 + 2.0 * s * t * 400.0 + t * t * 500.0,
                2.0 * s * t * 200.0,
            );
            worst = worst.max(dist(q2));
        }
        assert!(worst <= tol * (1.0 + 1e-9), "tol {tol}: worst {worst}");
        // Deterministic: a second run is bit-identical.
        assert_eq!(flatten(&path, &opts).unwrap(), lines);
    }
    // Hand value: cubic M = max |second differences| = |(0,0)-2(100,300)+(200,-300)| = 900 ->
    // n = ceil(sqrt(3*900/(4*1))) = 26; quad |d| = 400 -> n = ceil(sqrt(400/4)) = 10; 1 + 26 + 10.
    let one = FlattenOptions { tolerance: 1.0, ..FlattenOptions::default() };
    assert_eq!(flatten(&path, &one).unwrap()[0].points.len(), 37);
    for bad in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e-9, 1e7] {
        let o = FlattenOptions { tolerance: bad, ..FlattenOptions::default() };
        assert_eq!(flatten(&path, &o), Err(Error::UnsupportedOptions), "{bad}");
    }
    let tight = FlattenOptions { tolerance: 1e-6, max_points: 1000, ..FlattenOptions::default() };
    assert_eq!(flatten(&path, &tight), Err(Error::BudgetExceeded));
    let mut nan = path.clone();
    nan.elements[1] = PathElement::CubicTo(b, p(f64::NAN, 0.0), d);
    assert_eq!(flatten(&nan, &FlattenOptions::default()), Err(Error::InvalidGeometry));
    // Worst-case coordinates: 1e9 extremes at minimum tolerance are refused by budget, not hung.
    let mut huge = OwnedPath::new(FillRule::NonZero);
    huge.elements.push(PathElement::MoveTo(p(-1e9, -1e9)));
    huge.elements.push(PathElement::CubicTo(p(1e9, -1e9), p(-1e9, 1e9), p(1e9, 1e9)));
    let min_tol = FlattenOptions { tolerance: 1e-6, ..FlattenOptions::default() };
    assert_eq!(flatten(&huge, &min_tol), Err(Error::BudgetExceeded));
}

fn seg_dist(q: PathPoint, a: PathPoint, b: PathPoint) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 { 0.0 } else { (((q.x - a.x) * dx + (q.y - a.y) * dy) / len2).clamp(0.0, 1.0) };
    (q.x - (a.x + t * dx)).hypot(q.y - (a.y + t * dy))
}

#[test]
fn boolean_overlapping_squares_hand_values() {
    let (a, b) = (square(0.0, 0.0, 2.0), square(1.0, 1.0, 2.0));
    let cases = [
        (BooleanOp::Union, 7.0, 1),
        (BooleanOp::Intersect, 1.0, 1),
        (BooleanOp::Exclude, 6.0, 2),
        (BooleanOp::SubtractFront, 3.0, 1),
        (BooleanOp::SubtractBack, 3.0, 1),
    ];
    for (op, expected, contours) in cases {
        let r = boolean(&[a.clone(), b.clone()], op, &options(0.1)).unwrap();
        assert_eq!(area(&r.path), expected, "{}", op.wire());
        assert_eq!(r.receipt.output_contours, contours, "{}", op.wire());
        assert!(contour_areas(&r.path).iter().all(|a| *a > 0.0), "outer contours positive");
        assert_eq!(r.path.fill_rule, FillRule::NonZero);
        assert_eq!(BooleanOp::parse(op.wire()), Ok(op));
    }
    // Subtract: which operand survives depends on z-order only.
    let front = boolean(&[a.clone(), b.clone()], BooleanOp::SubtractFront, &options(0.1)).unwrap();
    assert!(front.path.elements.iter().any(|e| *e == PathElement::LineTo(p(0.0, 0.0)) || *e == PathElement::MoveTo(p(0.0, 0.0))));
    let back = boolean(&[a.clone(), b.clone()], BooleanOp::SubtractBack, &options(0.1)).unwrap();
    assert!(back.path.elements.iter().any(|e| *e == PathElement::LineTo(p(3.0, 3.0)) || *e == PathElement::MoveTo(p(3.0, 3.0))));
    // Union with a hole: frame minus inner square keeps a negative hole contour.
    let frame = boolean(&[square(0.0, 0.0, 4.0), square(1.0, 1.0, 2.0)], BooleanOp::SubtractFront, &options(0.1)).unwrap();
    let mut areas = contour_areas(&frame.path);
    areas.sort_by(f64::total_cmp);
    assert_eq!(areas, vec![-4.0, 16.0]);
}

#[test]
fn boolean_overlapping_circles_and_fill_rules() {
    // Two unit circles at distance 1: lens area 2 acos(1/2) - sqrt(3)/2.
    let lens = 2.0 * 0.5f64.acos() - 3f64.sqrt() / 2.0;
    let pi = std::f64::consts::PI;
    let (c1, c2) = (circle(0.0, 0.0, 1.0), circle(1.0, 0.0, 1.0));
    let tol = 1e-4;
    // The kappa arc deviates from the true circle by <= 2.7e-4 r (outward), the chords by <= tol.
    let slack = 2.0 * 2.0 * pi * (3e-4 + tol);
    let run = |op| boolean(&[c1.clone(), c2.clone()], op, &options(tol)).unwrap();
    let i = run(BooleanOp::Intersect);
    assert!((area(&i.path) - lens).abs() < slack, "intersect {}", area(&i.path));
    let u = run(BooleanOp::Union);
    assert!((area(&u.path) - (2.0 * pi - lens)).abs() < slack, "union {}", area(&u.path));
    let x = run(BooleanOp::Exclude);
    assert!((area(&x.path) - (2.0 * pi - 2.0 * lens)).abs() < slack, "exclude {}", area(&x.path));
    let s = run(BooleanOp::SubtractFront);
    assert!((area(&s.path) - (pi - lens)).abs() < slack, "subtract {}", area(&s.path));
    // Deterministic: canonical bytes of a repeated run are identical.
    assert_eq!(run(BooleanOp::Intersect).path.to_canonical_json(), i.path.to_canonical_json());
    assert!(i.receipt.grid_quantum > 0.0 && i.receipt.grid_quantum < 1e-8);
    // Three operands: intersect of three discs is inside every pairwise lens.
    let c3 = circle(0.5, 0.8, 1.0);
    let three = boolean(&[c1.clone(), c2.clone(), c3], BooleanOp::Intersect, &options(tol)).unwrap();
    assert!(area(&three.path) > 0.0 && area(&three.path) < lens);
    // Fill rules: a self-intersecting pentagram is a pentagon hole under even-odd.
    let star: Vec<(f64, f64)> = (0..5)
        .map(|k| {
            let t = pi / 2.0 + (k as f64) * 4.0 * pi / 5.0;
            (t.cos() * 10.0, t.sin() * 10.0)
        })
        .collect();
    let nz = normalise_path(&polygon(&star, FillRule::NonZero), &options(0.1)).unwrap();
    let eo = normalise_path(&polygon(&star, FillRule::EvenOdd), &options(0.1)).unwrap();
    // Hand values: pentagram outline area and its inner pentagon (circumradius 10).
    let r_in = 10.0 * (2.0 * pi / 5.0).cos() / (pi / 5.0).cos();
    let inner = 2.5 * r_in * r_in * (2.0 * pi / 5.0).sin();
    assert!((area(&nz.path) - area(&eo.path) - inner).abs() < 1e-3, "nz {} eo {} inner {inner}", area(&nz.path), area(&eo.path));
}

#[test]
fn boolean_hostile_inputs_are_typed() {
    let a = square(0.0, 0.0, 1.0);
    assert_eq!(boolean(std::slice::from_ref(&a), BooleanOp::Union, &options(0.1)), Err(Error::InvalidRequest));
    assert_eq!(
        boolean(&[a.clone(), square(5.0, 5.0, 1.0)], BooleanOp::Intersect, &options(0.1)),
        Err(Error::EmptyPathfinderResult)
    );
    assert_eq!(
        boolean(&[a.clone(), a.clone()], BooleanOp::SubtractFront, &options(0.1)),
        Err(Error::EmptyPathfinderResult)
    );
    let mut nan = a.clone();
    nan.elements[1] = PathElement::LineTo(p(f64::NAN, 1.0));
    assert_eq!(boolean(&[a.clone(), nan], BooleanOp::Union, &options(0.1)), Err(Error::InvalidGeometry));
    assert_eq!(BooleanOp::parse("divideCommand"), Err(Error::UnsupportedOperation));
    let mut few = options(0.1);
    few.max_operands = 2;
    assert_eq!(boolean(&[a.clone(), a.clone(), a.clone()], BooleanOp::Union, &few), Err(Error::BudgetExceeded));
    // Two combs of 200 crossing teeth: 40000 crossings exceed a 10000-pair candidate budget.
    let comb = |vertical: bool| {
        let mut pts = Vec::new();
        for k in 0..200 {
            let x = k as f64 * 2.0;
            pts.extend([(x, 0.0), (x, 400.0), (x + 1.0, 400.0), (x + 1.0, 0.0)]);
        }
        let pts: Vec<(f64, f64)> = if vertical { pts } else { pts.into_iter().map(|(x, y)| (y, x)).collect() };
        polygon(&pts, FillRule::NonZero)
    };
    let mut guard = options(0.1);
    guard.max_candidate_pairs = 10_000;
    assert_eq!(boolean(&[comb(true), comb(false)], BooleanOp::Union, &guard), Err(Error::BudgetExceeded));
    // Open subpaths fill closed; extreme coordinates stay exact on the grid.
    let mut open = OwnedPath::new(FillRule::NonZero);
    open.elements.push(PathElement::MoveTo(p(-1e9, -1e9)));
    open.elements.push(PathElement::LineTo(p(1e9, -1e9)));
    open.elements.push(PathElement::LineTo(p(1e9, 1e9)));
    let r = boolean(&[open, square(-1e9, -1e9, 1e9)], BooleanOp::Union, &options(0.1)).unwrap();
    assert!((area(&r.path) / 2.5e18 - 1.0).abs() < 1e-12, "{}", area(&r.path));
}
