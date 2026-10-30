//! Lane SMB-AI-ARROWHEADS-1 slice 2: Nib stroke end markers (STU-VEC-146). Marker vertices and
//! trimmed ends are computed by hand from the module's shape table (a Studio reading, UNVERIFIED D5).
use hsk_studio_nib::markers::*;
use hsk_studio_nib::{COORDINATE_LIMIT, FillRule, OwnedPath, PathElement, PathPoint};

const EPS: f64 = 1e-9;

fn p(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}
fn path(elements: Vec<PathElement>) -> OwnedPath {
    OwnedPath { elements, fill_rule: FillRule::NonZero }
}
fn line(a: PathPoint, b: PathPoint) -> OwnedPath {
    path(vec![PathElement::MoveTo(a), PathElement::LineTo(b)])
}
fn end(shape: MarkerShape, scale: f64) -> Option<MarkerEnd> {
    Some(MarkerEnd { shape, scale })
}
fn spec(start: Option<MarkerEnd>, end: Option<MarkerEnd>, align: MarkerAlign) -> MarkerSpec {
    MarkerSpec { start, end, align }
}
fn run(path: &OwnedPath, s: MarkerSpec, width: f64) -> Result<MarkerOutlines, MarkerError> {
    marker_outlines(path, &s, width, &MarkerOptions::default())
}
/// The vertices of a polygon outline (MoveTo/LineTo points, Close dropped).
fn vertices(o: &OwnedPath) -> Vec<PathPoint> {
    o.elements
        .iter()
        .filter_map(|e| match e {
            PathElement::MoveTo(q) | PathElement::LineTo(q) => Some(*q),
            _ => None,
        })
        .collect()
}
fn close(got: &[PathPoint], want: &[(f64, f64)]) {
    assert_eq!(got.len(), want.len(), "{got:?} vs {want:?}");
    for (g, w) in got.iter().zip(want) {
        assert!((g.x - w.0).abs() < EPS && (g.y - w.1).abs() < EPS, "{got:?} vs {want:?}");
    }
}
fn last_point(o: &OwnedPath) -> PathPoint {
    match o.elements.last() {
        Some(PathElement::LineTo(q)) | Some(PathElement::MoveTo(q)) => *q,
        other => panic!("not a line end: {other:?}"),
    }
}

#[test]
fn triangle_on_a_horizontal_line_inside_and_outside_the_path() {
    let l = line(p(0.0, 0.0), p(20.0, 0.0));
    // Width 2, scale 100: size s = 2; triangle tip (0,0), base (-3, +-1.5) in units of s.
    let inside = run(&l, spec(None, end(MarkerShape::Triangle, 100.0), MarkerAlign::InsidePath), 2.0).unwrap();
    assert_eq!(inside.markers.len(), 1);
    let m = &inside.markers[0];
    assert_eq!((m.subpath, m.at_start, m.size, m.direction), (0, false, 2.0, (1.0, 0.0)));
    close(&vertices(&m.outline), &[(20.0, 0.0), (14.0, 3.0), (14.0, -3.0)]);
    // The stroke stops at the base: trim 3 s = 6.
    close(&vertices(&inside.trimmed), &[(0.0, 0.0), (14.0, 0.0)]);
    let outside = run(&l, spec(None, end(MarkerShape::Triangle, 100.0), MarkerAlign::OutsidePath), 2.0).unwrap();
    close(&vertices(&outside.markers[0].outline), &[(26.0, 0.0), (20.0, 3.0), (20.0, -3.0)]);
    assert_eq!(outside.trimmed, l, "outside_path keeps the path");
    // The start marker points out of the path (direction reversed): tip at the start.
    let both = run(&l, spec(end(MarkerShape::Triangle, 100.0), end(MarkerShape::Triangle, 100.0), MarkerAlign::InsidePath), 2.0).unwrap();
    let s = both.markers.iter().find(|m| m.at_start).unwrap();
    assert_eq!(s.direction, (-1.0, 0.0));
    close(&vertices(&s.outline), &[(0.0, 0.0), (6.0, -3.0), (6.0, 3.0)]);
    close(&vertices(&both.trimmed), &[(6.0, 0.0), (14.0, 0.0)]);
}

#[test]
fn triangle_on_a_thirty_degree_line() {
    // End E = 20 (cos 30, sin 30); d = (cos 30, sin 30), n = (-sin 30, cos 30); s = 2.
    let (c, sn) = (0.866_025_403_784_438_6, 0.5);
    let e = (20.0 * c, 20.0 * sn);
    let l = line(p(0.0, 0.0), p(e.0, e.1));
    let inside = run(&l, spec(None, end(MarkerShape::Triangle, 100.0), MarkerAlign::InsidePath), 2.0).unwrap();
    // Base centre B = E - 6 d; base vertices B +- 3 n.
    let b = (e.0 - 6.0 * c, e.1 - 6.0 * sn);
    let want = [(e.0, e.1), (b.0 - 3.0 * sn, b.1 + 3.0 * c), (b.0 + 3.0 * sn, b.1 - 3.0 * c)];
    close(&vertices(&inside.markers[0].outline), &want);
    // By hand: B = (12.124355652982141, 7), vertices (10.624355652982141, 9.598076211353316) and
    // (13.624355652982141, 4.401923788646684).
    let by_hand = [(10.624_355_652_982_141, 9.598_076_211_353_316), (13.624_355_652_982_141, 4.401_923_788_646_684)];
    for (w, h) in want[1..].iter().zip(by_hand) {
        assert!((w.0 - h.0).abs() < EPS && (w.1 - h.1).abs() < EPS, "{w:?} vs {h:?}");
    }
    let t = last_point(&inside.trimmed);
    assert!((t.x - 12.124_355_652_982_141).abs() < EPS && (t.y - 7.0).abs() < EPS, "trimmed end {t:?}");
    let outside = run(&l, spec(None, end(MarkerShape::Triangle, 100.0), MarkerAlign::OutsidePath), 2.0).unwrap();
    let tip = (e.0 + 6.0 * c, e.1 + 6.0 * sn);
    close(
        &vertices(&outside.markers[0].outline),
        &[tip, (e.0 - 3.0 * sn, e.1 + 3.0 * c), (e.0 + 3.0 * sn, e.1 - 3.0 * c)],
    );
    assert_eq!(outside.trimmed, l);
}

#[test]
fn scale_sets_the_marker_size_at_each_end() {
    let l = line(p(0.0, 0.0), p(40.0, 0.0));
    // Width 2: scale 50 -> s = 1 at the start, scale 300 -> s = 6 at the end.
    let out = run(&l, spec(end(MarkerShape::Triangle, 50.0), end(MarkerShape::Triangle, 300.0), MarkerAlign::InsidePath), 2.0).unwrap();
    let s = out.markers.iter().find(|m| m.at_start).unwrap();
    let e = out.markers.iter().find(|m| !m.at_start).unwrap();
    assert_eq!((s.size, e.size), (1.0, 6.0));
    close(&vertices(&s.outline), &[(0.0, 0.0), (3.0, -1.5), (3.0, 1.5)]);
    close(&vertices(&e.outline), &[(40.0, 0.0), (22.0, 9.0), (22.0, -9.0)]);
    close(&vertices(&out.trimmed), &[(3.0, 0.0), (22.0, 0.0)]);
    // Every drawn shape's outline lies within its declared length behind the tip (scale 300, s = 6).
    for shape in MarkerShape::ALL {
        let o = run(&l, spec(None, end(shape, 300.0), MarkerAlign::InsidePath), 2.0).unwrap();
        let m = &o.markers[0];
        assert!(m.outline.is_closed(), "{shape:?} outline closed");
        let reach = shape.length() * 6.0;
        for q in m.outline.elements.iter().flat_map(|el| match *el {
            PathElement::MoveTo(a) | PathElement::LineTo(a) => vec![a],
            PathElement::QuadTo(a, b) => vec![a, b],
            PathElement::CubicTo(a, b, c) => vec![a, b, c],
            PathElement::Close => vec![],
        }) {
            assert!(q.x <= 40.0 + EPS && q.x >= 40.0 - reach - EPS, "{shape:?} {q:?}");
        }
        assert_eq!(MarkerShape::from_token(shape.token()), Ok(Some(shape)));
        let trimmed_end = last_point(&o.trimmed).x;
        assert!((trimmed_end - (40.0 - shape.trim() * 6.0)).abs() < EPS, "{shape:?} trim");
    }
}

#[test]
fn closed_subpaths_report_no_ends() {
    let square = vec![
        PathElement::MoveTo(p(0.0, 0.0)),
        PathElement::LineTo(p(10.0, 0.0)),
        PathElement::LineTo(p(10.0, 10.0)),
        PathElement::LineTo(p(0.0, 10.0)),
        PathElement::Close,
    ];
    let mut elements = square.clone();
    elements.extend([PathElement::MoveTo(p(20.0, 0.0)), PathElement::LineTo(p(40.0, 0.0))]);
    let both = spec(end(MarkerShape::Triangle, 100.0), end(MarkerShape::Triangle, 100.0), MarkerAlign::InsidePath);
    let out = run(&path(elements), both, 2.0).unwrap();
    assert_eq!(out.closed_subpaths, vec![0]);
    assert!(out.markers.iter().all(|m| m.subpath == 1));
    assert_eq!(out.markers.len(), 2);
    assert_eq!(&out.trimmed.elements[..5], &square[..], "the closed subpath is kept verbatim");
    let only_closed = run(&path(square), both, 2.0).unwrap();
    assert!(only_closed.markers.is_empty());
    assert_eq!(only_closed.closed_subpaths, vec![0]);
    // Trims that meet consume the open subpath, which is reported.
    let short = run(&line(p(0.0, 0.0), p(10.0, 0.0)), both, 2.0).unwrap();
    assert_eq!((short.consumed_subpaths.clone(), short.markers.len()), (vec![0], 2));
    assert!(short.trimmed.elements.is_empty());
}

#[test]
fn hostile_marker_input_is_refused_typed() {
    let l = line(p(0.0, 0.0), p(20.0, 0.0));
    let tri = |scale| spec(None, end(MarkerShape::Triangle, scale), MarkerAlign::InsidePath);
    assert_eq!(run(&line(p(0.0, 0.0), p(f64::NAN, 0.0)), tri(100.0), 2.0).err(), Some(MarkerError::InvalidGeometry));
    assert_eq!(run(&line(p(5.0, 5.0), p(5.0, 5.0)), tri(100.0), 2.0).err(), Some(MarkerError::DegenerateTangent));
    assert_eq!(run(&path(vec![PathElement::MoveTo(p(5.0, 5.0))]), tri(100.0), 2.0).err(), Some(MarkerError::DegenerateTangent));
    for bad in [0.0, 0.999, 1000.001, f64::NAN, f64::INFINITY, -100.0] {
        assert_eq!(run(&l, tri(bad), 2.0).err(), Some(MarkerError::ScaleOutOfRange), "scale {bad}");
    }
    assert!(run(&l, tri(1.0), 2.0).is_ok() && run(&l, tri(1000.0), 2.0).is_ok(), "hard bounds 1 and 1000");
    for w in [0.0, -2.0, f64::NAN, COORDINATE_LIMIT * 2.0] {
        assert_eq!(run(&l, tri(100.0), w).err(), Some(MarkerError::InvalidWidth), "width {w}");
    }
    // A marker beyond the coordinate limit is refused, not clamped.
    let far = line(p(0.0, 0.0), p(COORDINATE_LIMIT, 0.0));
    assert_eq!(run(&far, spec(None, end(MarkerShape::Triangle, 1000.0), MarkerAlign::OutsidePath), 1.0e6).err(), Some(MarkerError::OutOfRange));
    // Tokens: none is no marker, the connector terminators are unbuilt, unknown text is refused.
    assert_eq!(MarkerShape::from_token("arrow.none"), Ok(None));
    assert_eq!(MarkerShape::from_token("NONE"), Ok(None));
    for t in ["ERD_ONE", "ERD_MANY", "ERD_ONE_OR_MORE", "ERD_ZERO_OR_MORE", "ERD_ZERO_OR_ONE", "ERD_EXACTLY_ONE"] {
        assert_eq!(MarkerShape::from_token(t), Err(MarkerError::ShapeUnbuilt), "{t}");
    }
    assert_eq!(MarkerShape::from_token("arrow.Triangle"), Err(MarkerError::UnknownToken));
    assert_eq!(MarkerError::ShapeUnbuilt.code(), "marker_shape_unbuilt");
    // A degenerate end tangent (last handle on the end point) uses the next derivative: p3 - p1.
    let curve = path(vec![
        PathElement::MoveTo(p(0.0, 0.0)),
        PathElement::CubicTo(p(10.0, 10.0), p(20.0, 0.0), p(20.0, 0.0)),
    ]);
    let o = run(&curve, tri(100.0), 2.0).unwrap();
    let d = o.markers[0].direction;
    let k = 1.0 / 200.0_f64.sqrt();
    assert!((d.0 - 10.0 * k).abs() < EPS && (d.1 + 10.0 * k).abs() < EPS, "{d:?}");
    // A trailing zero-length segment is skipped for the direction.
    let tail = path(vec![PathElement::MoveTo(p(0.0, 0.0)), PathElement::LineTo(p(20.0, 0.0)), PathElement::LineTo(p(20.0, 0.0))]);
    assert_eq!(run(&tail, tri(100.0), 2.0).unwrap().markers[0].direction, (1.0, 0.0));
    // Options are checked.
    let opts = MarkerOptions { arclen_accuracy: 0.0, ..MarkerOptions::default() };
    assert_eq!(marker_outlines(&l, &tri(100.0), 2.0, &opts).err(), Some(MarkerError::InvalidOptions));
}
