//! Lane SMB-NIB-VECTOR-RENDER slice 2: stroke outlining (width, caps, joins, miter limit, dashes,
//! align) checked against hand-computed areas.
use hsk_studio_nib::stroke::{
    DashPattern, LineCap, LineJoin, StrokeAlign, StrokeOptions, StrokeStyle, outline_stroke,
};
use hsk_studio_nib::{Error, FillRule, OwnedPath, PathElement, PathPoint};
use std::f64::consts::PI;

fn p(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}

fn polyline(points: &[(f64, f64)], closed: bool) -> OwnedPath {
    let mut path = OwnedPath::new(FillRule::NonZero);
    for (i, &(x, y)) in points.iter().enumerate() {
        path.elements.push(if i == 0 {
            PathElement::MoveTo(p(x, y))
        } else {
            PathElement::LineTo(p(x, y))
        });
    }
    if closed {
        path.elements.push(PathElement::Close);
    }
    path
}

fn contour_areas(path: &OwnedPath) -> Vec<f64> {
    let mut out = Vec::new();
    let mut pts: Vec<PathPoint> = Vec::new();
    for e in &path.elements {
        match *e {
            PathElement::MoveTo(q) => pts = vec![q],
            PathElement::LineTo(q) => pts.push(q),
            PathElement::Close => {
                let n = pts.len();
                let a: f64 = (0..n)
                    .map(|i| pts[i].x * pts[(i + 1) % n].y - pts[(i + 1) % n].x * pts[i].y)
                    .sum();
                out.push(a / 2.0);
            }
            _ => panic!("outline must be polygonal"),
        }
    }
    out
}

fn area(path: &OwnedPath) -> f64 {
    contour_areas(path).iter().sum()
}

fn stroke(path: &OwnedPath, style: &StrokeStyle) -> Result<(f64, usize), Error> {
    let r = outline_stroke(path, style, &StrokeOptions::default())?;
    // Deterministic: a second run yields identical canonical bytes.
    let again = outline_stroke(path, style, &StrokeOptions::default())?;
    assert_eq!(r.path.to_canonical_json(), again.path.to_canonical_json());
    Ok((area(&r.path), contour_areas(&r.path).len()))
}

fn close(a: f64, b: f64, eps: f64) -> bool {
    (a - b).abs() <= eps
}

#[test]
fn stroke_caps_on_a_line() {
    let line = polyline(&[(0.0, 0.0), (10.0, 0.0)], false);
    let mut style = StrokeStyle::new(2.0);
    assert_eq!(stroke(&line, &style).unwrap(), (20.0, 1));
    style.cap = LineCap::Square;
    assert_eq!(stroke(&line, &style).unwrap(), (24.0, 1));
    style.cap = LineCap::Round;
    let (a, n) = stroke(&line, &style).unwrap();
    // Two half discs of radius 1; chords lie inside the circle within tolerance 0.1.
    assert!(a < 20.0 + PI && a > 20.0 + PI - 2.0 * PI * 0.1, "{a}");
    assert_eq!(n, 1);
    // The butt outline is exactly the rectangle.
    let r = outline_stroke(&line, &StrokeStyle::new(2.0), &StrokeOptions::default()).unwrap();
    let mut pts: Vec<(f64, f64)> = r
        .path
        .elements
        .iter()
        .filter_map(|e| match e {
            PathElement::MoveTo(q) | PathElement::LineTo(q) => Some((q.x, q.y)),
            _ => None,
        })
        .collect();
    pts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert_eq!(pts, vec![(0.0, -1.0), (0.0, 1.0), (10.0, -1.0), (10.0, 1.0)]);
}

#[test]
fn stroke_joins_and_miter_limit() {
    // Left turn at (10,0): horizontal [0,10]x[-1,1] + vertical [9,11]x[0,10] overlap 1 -> 39.
    let corner = polyline(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], false);
    let mut style = StrokeStyle::new(2.0);
    style.join = LineJoin::Miter;
    assert_eq!(stroke(&corner, &style).unwrap().0, 40.0); // + unit square to the tip (11,-1)
    style.join = LineJoin::Bevel;
    assert_eq!(stroke(&corner, &style).unwrap().0, 39.5); // + half unit triangle
    style.join = LineJoin::Round;
    let (a, _) = stroke(&corner, &style).unwrap();
    assert!(a < 39.0 + PI / 4.0 && a > 39.0 + PI / 4.0 - 0.2, "{a}");
    // 90 degree miter ratio is sqrt(2) = 1.414..: limit 1.4 bevels, limit 1.5 mitres.
    style.join = LineJoin::Miter;
    style.miter_limit = 1.4;
    assert_eq!(stroke(&corner, &style).unwrap().0, 39.5);
    style.miter_limit = 1.5;
    assert_eq!(stroke(&corner, &style).unwrap().0, 40.0);
    // Right turn mirrors: the tip is on the other side, same area.
    let right = polyline(&[(0.0, 0.0), (10.0, 0.0), (10.0, -10.0)], false);
    assert_eq!(stroke(&right, &style).unwrap().0, 40.0);
    // A 10 degree spike exceeds the default limit 4 and bevels: its area stays below the miter one.
    let spike = polyline(&[(0.0, 0.0), (100.0, 0.0), (0.0, 100.0 * (10f64).to_radians().tan())], false);
    let mut bevel = StrokeStyle::new(2.0);
    bevel.join = LineJoin::Bevel;
    let mut miter = StrokeStyle::new(2.0);
    miter.miter_limit = 4.0;
    assert_eq!(stroke(&spike, &miter).unwrap().0, stroke(&spike, &bevel).unwrap().0);
    miter.miter_limit = 500.0;
    assert!(stroke(&spike, &miter).unwrap().0 > stroke(&spike, &bevel).unwrap().0);
}

#[test]
fn stroke_dashes_hand_values() {
    let line = polyline(&[(0.0, 0.0), (10.0, 0.0)], false);
    let dashed = |lengths: Vec<f64>, phase: f64, cap: LineCap| {
        let mut style = StrokeStyle::new(2.0);
        style.cap = cap;
        style.dash = Some(DashPattern { lengths, phase });
        stroke(&line, &style).unwrap()
    };
    // [2,1]: on [0,2] [3,5] [6,8] [9,10] -> 7 long, width 2.
    assert_eq!(dashed(vec![2.0, 1.0], 0.0, LineCap::Butt), (14.0, 4));
    // phase 1: on [0,1] [2,4] [5,7] [8,10].
    assert_eq!(dashed(vec![2.0, 1.0], 1.0, LineCap::Butt), (14.0, 4));
    // Negative phase wraps: -2 == phase 1 of a period 3.
    assert_eq!(dashed(vec![2.0, 1.0], -2.0, LineCap::Butt), (14.0, 4));
    // Odd list repeats: [1] == [1,1] -> on [0,1] [2,3] .. [8,9].
    assert_eq!(dashed(vec![1.0], 0.0, LineCap::Butt), (10.0, 5));
    // Zero-length dashes with round caps: dots at 0, 3, 6, 9 (4 unit discs).
    let (a, n) = dashed(vec![0.0, 3.0], 0.0, LineCap::Round);
    assert_eq!(n, 4);
    assert!(a < 4.0 * PI && a > 4.0 * PI - 4.0 * 2.0 * PI * 0.1, "{a}");
    // A dash starting exactly at the end of the line has no length: no end dot with round caps.
    // (width 0.5 so the round caps of neighbouring dashes stay apart: 5 dashes, not 6 contours.)
    let mut thin = StrokeStyle::new(0.5);
    thin.cap = LineCap::Round;
    thin.dash = Some(DashPattern { lengths: vec![1.0], phase: 0.0 });
    let (a, n) = stroke(&line, &thin).unwrap();
    assert_eq!(n, 5);
    assert!(a < 5.0 * (0.5 + PI * 0.0625), "{a}");
    // Zero-length dashes with butt caps draw nothing: typed empty result.
    let mut style = StrokeStyle::new(2.0);
    style.dash = Some(DashPattern { lengths: vec![0.0, 2.0], phase: 0.0 });
    assert_eq!(
        outline_stroke(&line, &style, &StrokeOptions::default()).err(),
        Some(Error::EmptyPathfinderResult)
    );
    // A dash across a corner keeps its join: [30] over the miter corner equals the solid stroke.
    let corner = polyline(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)], false);
    let mut solid = StrokeStyle::new(2.0);
    solid.dash = Some(DashPattern { lengths: vec![30.0, 5.0], phase: 0.0 });
    assert_eq!(stroke(&corner, &solid).unwrap().0, 40.0);
}

#[test]
fn stroke_closed_paths_align_and_curves() {
    let square = polyline(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)], true);
    let mut style = StrokeStyle::new(2.0);
    let (a, n) = stroke(&square, &style).unwrap();
    assert_eq!((a, n), (12.0 * 12.0 - 8.0 * 8.0, 2)); // ring: outer + hole
    style.align = StrokeAlign::Inside;
    // Inside: a band of width 2 inside the square ([0,10]^2 minus [2,8]^2).
    assert_eq!(stroke(&square, &style).unwrap().0, 100.0 - 36.0);
    // Outside: a band of width 2 outside it ([-2,12]^2 minus [0,10]^2, mitred corners).
    style.align = StrokeAlign::Outside;
    assert_eq!(stroke(&square, &style).unwrap().0, 196.0 - 100.0);
    // Open paths have no inside: align is refused, not guessed.
    let open = polyline(&[(0.0, 0.0), (10.0, 0.0)], false);
    assert_eq!(
        outline_stroke(&open, &style, &StrokeOptions::default()).err(),
        Some(Error::UnsupportedOptions)
    );
    // Circle r=10 (four kappa cubics), width 2: annulus 40 pi within tolerance.
    let (r, k) = (10.0, 0.552_284_749_830_793_4 * 10.0);
    let mut circle = OwnedPath::new(FillRule::NonZero);
    circle.elements.push(PathElement::MoveTo(p(r, 0.0)));
    circle.elements.push(PathElement::CubicTo(p(r, k), p(k, r), p(0.0, r)));
    circle.elements.push(PathElement::CubicTo(p(-k, r), p(-r, k), p(-r, 0.0)));
    circle.elements.push(PathElement::CubicTo(p(-r, -k), p(-k, -r), p(0.0, -r)));
    circle.elements.push(PathElement::CubicTo(p(k, -r), p(r, -k), p(r, 0.0)));
    circle.elements.push(PathElement::Close);
    let (a, n) = stroke(&circle, &StrokeStyle::new(2.0)).unwrap();
    assert_eq!(n, 2);
    assert!(close(a, 40.0 * PI, 2.0 * PI * 22.0 * 0.1), "{a}");
}

#[test]
fn stroke_hostile_inputs_and_wire_values() {
    let line = polyline(&[(0.0, 0.0), (10.0, 0.0)], false);
    let opts = StrokeOptions::default();
    for width in [0.0, -1.0, f64::NAN, f64::INFINITY, 2e9] {
        let s = StrokeStyle::new(width);
        assert_eq!(outline_stroke(&line, &s, &opts).err(), Some(Error::UnsupportedOptions), "{width}");
    }
    for limit in [0.5, 500.5, f64::NAN] {
        let mut s = StrokeStyle::new(1.0);
        s.miter_limit = limit;
        assert_eq!(outline_stroke(&line, &s, &opts).err(), Some(Error::UnsupportedOptions));
    }
    for lengths in [vec![], vec![0.0, 0.0], vec![1.0, -1.0], vec![f64::NAN]] {
        let mut s = StrokeStyle::new(1.0);
        s.dash = Some(DashPattern { lengths, phase: 0.0 });
        assert_eq!(outline_stroke(&line, &s, &opts).err(), Some(Error::UnsupportedOptions));
    }
    // A pattern far finer than the path length is refused before any dash is built.
    let long = polyline(&[(-1e9, 0.0), (1e9, 0.0)], false);
    let mut s = StrokeStyle::new(1.0);
    s.dash = Some(DashPattern { lengths: vec![1e-3, 1e-3], phase: 0.0 });
    assert_eq!(outline_stroke(&long, &s, &opts).err(), Some(Error::BudgetExceeded));
    // Round joins at a huge radius with the minimum tolerance are refused by the point budget.
    let mut tight = StrokeOptions::default();
    tight.geometry.flatten.tolerance = 1e-6;
    let mut round = StrokeStyle::new(1e9);
    round.cap = LineCap::Round;
    assert_eq!(outline_stroke(&line, &round, &tight).err(), Some(Error::BudgetExceeded));
    let mut nan = line.clone();
    nan.elements[1] = PathElement::LineTo(p(f64::NAN, 0.0));
    assert_eq!(outline_stroke(&nan, &StrokeStyle::new(1.0), &opts).err(), Some(Error::InvalidGeometry));
    // Wire values (STU-VEC-143, STU-VEC-113).
    for (cap, v) in [(LineCap::Butt, 1), (LineCap::Round, 2), (LineCap::Square, 3)] {
        assert_eq!((cap.wire(), LineCap::from_wire(v)), (v, Ok(cap)));
    }
    for (join, s, o) in [(LineJoin::Miter, 1, 0), (LineJoin::Round, 2, 1), (LineJoin::Bevel, 3, 2)] {
        assert_eq!((join.stroke_wire(), join.offset_wire()), (s, o));
        assert_eq!((LineJoin::from_stroke_wire(s), LineJoin::from_offset_wire(o)), (Ok(join), Ok(join)));
    }
    assert_eq!(LineCap::from_wire(0), Err(Error::InvalidEncoding));
    assert_eq!(LineJoin::from_stroke_wire(0), Err(Error::InvalidEncoding));
    for align in [StrokeAlign::Center, StrokeAlign::Inside, StrokeAlign::Outside] {
        assert_eq!(StrokeAlign::parse(align.wire()), Ok(align));
    }
    assert_eq!(StrokeAlign::parse("center"), Err(Error::InvalidEncoding));
}
