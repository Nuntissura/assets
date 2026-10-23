//! Lane SMB-ILLUSTRATOR-PEN-3: network planarize (crossings, T-junctions), knife on strokes,
//! variable-width outlines, reshape and blob brush against hand-built geometry.
use hsk_studio_nib::anchor_path::{AnchorError, AnchorPath, Contour, EditAnchor, point_at};
use hsk_studio_nib::boolean::BooleanOptions;
use hsk_studio_nib::flatten::FlattenOptions;
use hsk_studio_nib::freeform::{blob_brush, knife_strokes, reshape};
use hsk_studio_nib::network::{NetworkError, NetworkRegion, NetworkSegment, NetworkVertex, VectorNetwork};
use hsk_studio_nib::shape::{EllipseSpec, RectSpec, ellipse, rectangle};
use hsk_studio_nib::width::{WidthCap, WidthPoint, WidthProfile, outline_variable};
use hsk_studio_nib::{Error, FillRule, OwnedPath, PathElement as E, PathPoint};
use std::f64::consts::PI;

fn p(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
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

fn contour_area(c: &Contour) -> f64 {
    let n = c.anchors.len();
    (0..n).map(|i| c.anchors[i].position.x * c.anchors[(i + 1) % n].position.y - c.anchors[(i + 1) % n].position.x * c.anchors[i].position.y).sum::<f64>().abs() / 2.0
}

fn fine() -> BooleanOptions {
    BooleanOptions { flatten: FlattenOptions { tolerance: 0.001, ..BooleanOptions::default().flatten }, ..BooleanOptions::default() }
}

fn square_net(extra: &[(f64, f64)], extra_segments: &[(usize, usize)]) -> VectorNetwork {
    let mut vertices = vec![NetworkVertex::at(0.0, 0.0), NetworkVertex::at(10.0, 0.0), NetworkVertex::at(10.0, 10.0), NetworkVertex::at(0.0, 10.0)];
    vertices.extend(extra.iter().map(|(x, y)| NetworkVertex::at(*x, *y)));
    let mut segments = vec![NetworkSegment::line(0, 1), NetworkSegment::line(1, 2), NetworkSegment::line(2, 3), NetworkSegment::line(3, 0)];
    segments.extend(extra_segments.iter().map(|(a, b)| NetworkSegment::line(*a, *b)));
    VectorNetwork { vertices, segments, regions: vec![] }
}

#[test]
fn planarize_splits_crossings_and_t_junctions() {
    // Diagonals that cross without a shared vertex: one new vertex, four triangles.
    let x = square_net(&[], &[(0, 2), (1, 3)]);
    assert_ne!(x.detect_regions(true).unwrap().len(), 4, "unsplit: the crossing is invisible to faces");
    let px = x.planarize(1e-6).unwrap();
    assert_eq!((px.vertices.len(), px.segments.len()), (5, 8));
    assert_eq!(px.vertices[4], NetworkVertex::at(5.0, 5.0));
    let faces = px.detect_regions(true).unwrap();
    assert_eq!(faces.len(), 4);
    let net = VectorNetwork { regions: faces, ..px };
    for f in net.fill_geometry().unwrap() {
        assert!((contour_area(&f.contours[0]) - 25.0).abs() < 1e-9);
    }
    // T-junctions: E(5,0) and F(5,10) lie on edges AB and CD; the segment E-F splits the square in two.
    let mut t = square_net(&[(5.0, 0.0), (5.0, 10.0)], &[(4, 5)]);
    t.regions.push(NetworkRegion { winding_rule: FillRule::NonZero, loops: vec![vec![0, 1, 2, 3]], fills: true, fill_style_id: None });
    let pt = t.planarize(1e-6).unwrap();
    assert_eq!(pt.segments.len(), 7);
    assert_eq!(pt.regions[0].loops[0].len(), 6, "the existing region is remapped onto the split edges");
    assert!((contour_area(&pt.fill_geometry().unwrap()[0].contours[0]) - 100.0).abs() < 1e-9);
    assert_eq!(pt.detect_regions(true).unwrap().len(), 2);
    // A near-miss end within tolerance snaps as a T-junction.
    let near = square_net(&[(5.0, 0.0004), (5.0, 9.9996)], &[(4, 5)]);
    assert_eq!(near.planarize(1e-3).unwrap().detect_regions(true).unwrap().len(), 2);
    assert_eq!(near.planarize(1e-6).unwrap().detect_regions(true).unwrap().len(), 1, "outside tolerance: no junction");
    // Typed refusals.
    assert_eq!(x.planarize(0.0).unwrap_err(), NetworkError::OutOfRange);
    assert_eq!(x.planarize(f64::NAN).unwrap_err(), NetworkError::NonFinite);
}

#[test]
fn planarize_curves_and_knife_strokes() {
    // A circle (promoted path) crossed by a diameter: two half discs.
    let circle = ellipse(&EllipseSpec::full(0.0, 0.0, 10.0, 10.0)).unwrap();
    let mut net = VectorNetwork::from_path(&circle).unwrap();
    net.regions.clear();
    let a = net.vertices.len();
    net.vertices.extend([NetworkVertex::at(-15.0, 0.5), NetworkVertex::at(15.0, 0.5)]);
    net.segments.push(NetworkSegment::line(a, a + 1));
    let faces = net.planarize(1e-6).unwrap();
    let regions = faces.detect_regions(true).unwrap();
    assert_eq!(regions.len(), 2);
    let filled = VectorNetwork { regions, ..faces };
    let mut halves: Vec<f64> = filled.fill_geometry().unwrap().iter().map(|g| area(&hsk_studio_nib::boolean::normalise_path(&g.to_owned_path().unwrap(), &fine()).unwrap().path).abs()).collect();
    halves.sort_by(f64::total_cmp);
    let total: f64 = halves.iter().sum();
    assert!((total - PI * 100.0).abs() < 1e-3 * PI * 100.0, "{halves:?}");
    assert!(halves[0] < halves[1], "the chord at y = 0.5 is off centre");

    // Knife on an open line: two crossings, three open pieces sharing the crossing points.
    let line = AnchorPath { contours: vec![Contour { anchors: vec![EditAnchor::corner(0.0, 0.0), EditAnchor::corner(30.0, 0.0)], closed: false }], fill_rule: FillRule::NonZero };
    let pieces = knife_strokes(&line, &[p(10.0, -5.0), p(10.0, 5.0), p(20.0, 5.0), p(20.0, -5.0)]).unwrap();
    let ends: Vec<(PathPoint, PathPoint)> = pieces.iter().map(|q| (q.contours[0].anchors[0].position, q.contours[0].anchors.last().unwrap().position)).collect();
    assert_eq!(ends, vec![(p(0.0, 0.0), p(10.0, 0.0)), (p(10.0, 0.0), p(20.0, 0.0)), (p(20.0, 0.0), p(30.0, 0.0))]);
    assert!(pieces.iter().all(|q| !q.contours[0].closed));
    // A curve cut once: the two pieces meet on the curve.
    let mut s = EditAnchor::corner(0.0, 0.0);
    s.outgoing = p(0.0, 10.0);
    let mut e = EditAnchor::corner(10.0, 0.0);
    e.incoming = p(0.0, 10.0);
    let arch = AnchorPath { contours: vec![Contour { anchors: vec![s, e], closed: false }], fill_rule: FillRule::NonZero };
    let cut = knife_strokes(&arch, &[p(5.0, -1.0), p(5.0, 20.0)]).unwrap();
    assert_eq!(cut.len(), 2);
    let joint = cut[0].contours[0].anchors.last().unwrap().position;
    assert!((joint.x - 5.0).abs() < 0.05 && (joint.y - point_at(&arch, 0, 0, 0.5).unwrap().y).abs() < 0.05, "{joint:?}");
    // A closed square cut by a line through two edges: two open pieces.
    let sq = rectangle(&RectSpec::plain(0.0, 0.0, 10.0, 10.0)).unwrap().0;
    assert_eq!(knife_strokes(&sq, &[p(5.0, -1.0), p(5.0, 11.0)]).unwrap().len(), 2);
    // A miss leaves the contour; a one-point cut is refused.
    assert_eq!(knife_strokes(&line, &[p(50.0, -5.0), p(50.0, 5.0)]).unwrap(), vec![line.clone()]);
    assert_eq!(knife_strokes(&line, &[p(1.0, 1.0)]).unwrap_err(), AnchorError::InvalidParameter);
}

#[test]
fn variable_width_outlines() {
    let opts = fine();
    let line = OwnedPath { elements: vec![E::MoveTo(p(0.0, 0.0)), E::LineTo(p(20.0, 0.0))], fill_rule: FillRule::NonZero };
    // Uniform 4: a 20 x 4 rectangle; tapered 0 -> 4: a triangle of area 40.
    let uniform = outline_variable(&line, &WidthProfile::uniform(4.0), WidthCap::Butt, &opts).unwrap();
    assert!((area(&uniform.path).abs() - 80.0).abs() < 1e-6);
    let taper = WidthProfile { points: vec![WidthPoint { t: 0.0, width: 0.0 }, WidthPoint { t: 1.0, width: 4.0 }] };
    assert!((area(&outline_variable(&line, &taper, WidthCap::Butt, &opts).unwrap().path).abs() - 40.0).abs() < 1e-6);
    // Round caps add a disc of the end widths: 80 + pi * 2^2.
    let round = area(&outline_variable(&line, &WidthProfile::uniform(4.0), WidthCap::Round, &opts).unwrap().path).abs();
    assert!((round - (80.0 + PI * 4.0)).abs() < 0.02, "{round}");
    // A closed circle r = 10 with width 2: an annulus of area pi (11^2 - 9^2).
    let circle = ellipse(&EllipseSpec::full(0.0, 0.0, 10.0, 10.0)).unwrap().to_owned_path().unwrap();
    let ring = area(&outline_variable(&circle, &WidthProfile::uniform(2.0), WidthCap::Butt, &opts).unwrap().path).abs();
    assert!((ring - PI * 40.0).abs() < 0.01 * PI * 40.0, "{ring}");
    // Profile editing and validation.
    let edited = WidthProfile::uniform(2.0).with_point(0.5, 6.0).unwrap();
    assert_eq!(edited.points.len(), 3);
    assert_eq!(edited.width_at(0.25), 4.0);
    assert_eq!(edited.with_point(0.5, 1.0).unwrap().points.len(), 3, "replaces the point at t");
    assert_eq!(WidthProfile { points: vec![WidthPoint { t: 0.5, width: 1.0 }, WidthPoint { t: 0.2, width: 1.0 }] }.validate(), Err(Error::UnsupportedOptions));
    assert_eq!(WidthProfile::uniform(-1.0).validate(), Err(Error::UnsupportedOptions));
    assert_eq!(WidthProfile { points: vec![] }.validate(), Err(Error::UnsupportedOptions));
    assert_eq!(WidthProfile::uniform(2.0).with_point(f64::NAN, 1.0).unwrap_err(), Error::UnsupportedOptions);
}

#[test]
fn reshape_keeps_continuity_and_lands_on_target() {
    let smooth = |x: f64, dx: f64| EditAnchor::smooth(x, 0.0, dx, 0.0);
    let path = AnchorPath { contours: vec![Contour { anchors: vec![EditAnchor::corner(0.0, 0.0), smooth(20.0, 5.0), EditAnchor::corner(40.0, 0.0)], closed: false }], fill_rule: FillRule::NonZero };
    let target = { let q = point_at(&path, 0, 0, 0.5).unwrap(); p(q.x, q.y + 8.0) };
    let r = reshape(&path, 0, 0, 0.5, 0.0, 8.0, 100.0).unwrap();
    let got = point_at(&r, 0, 0, 0.5).unwrap();
    assert!((got.x - target.x).abs() < 1e-9 && (got.y - target.y).abs() < 1e-9, "{got:?}");
    // Every anchor within the radius moved up; the smooth anchor keeps collinear handles.
    assert!(r.contours[0].anchors.iter().all(|a| a.position.y > 0.0));
    let mid = r.contours[0].anchors[1];
    assert!((mid.incoming.x * mid.outgoing.y - mid.incoming.y * mid.outgoing.x).abs() < 1e-9, "continuity kept: {mid:?}");
    // A tiny radius moves no anchor: only the segment bends.
    let local = reshape(&path, 0, 0, 0.5, 0.0, 8.0, 1.0).unwrap();
    assert_eq!(local.contours[0].anchors.iter().map(|a| a.position).collect::<Vec<_>>(), path.contours[0].anchors.iter().map(|a| a.position).collect::<Vec<_>>());
    let got = point_at(&local, 0, 0, 0.5).unwrap();
    assert!((got.y - target.y).abs() < 1e-9);
    // Typed refusals.
    assert_eq!(reshape(&path, 0, 0, 0.0, 1.0, 1.0, 10.0).unwrap_err(), AnchorError::InvalidParameter);
    assert_eq!(reshape(&path, 0, 5, 0.5, 1.0, 1.0, 10.0).unwrap_err(), AnchorError::PointIndexOutOfBounds);
    assert_eq!(reshape(&path, 0, 0, 0.5, f64::NAN, 1.0, 10.0).unwrap_err(), AnchorError::NonFinite);
}

#[test]
fn blob_brush_paints_and_merges_same_attribute_shapes() {
    let opts = fine();
    let (stroke, merged) = blob_brush(&[p(0.0, 0.0), p(10.0, 0.0)], 4.0, &[], &opts).unwrap();
    assert!(merged.is_empty());
    assert!((area(&stroke).abs() - (40.0 + PI * 4.0)).abs() < 0.05, "{}", area(&stroke));
    // It merges with an overlapping square and leaves a far one alone.
    let near = rectangle(&RectSpec::plain(8.0, -5.0, 10.0, 10.0)).unwrap().0.to_owned_path().unwrap();
    let far = rectangle(&RectSpec::plain(100.0, 100.0, 10.0, 10.0)).unwrap().0.to_owned_path().unwrap();
    let (united, merged) = blob_brush(&[p(0.0, 0.0), p(10.0, 0.0)], 4.0, &[far, near], &opts).unwrap();
    assert_eq!(merged, vec![1]);
    let a = area(&united).abs();
    // Square 100 + the stroke part left of x = 8 (8 x 4 + half disc 2 pi).
    assert!((a - (100.0 + 32.0 + 2.0 * PI)).abs() < 0.05, "{a}");
    // Refusals.
    assert_eq!(blob_brush(&[], 4.0, &[], &opts).unwrap_err(), Error::InvalidRequest);
    assert_eq!(blob_brush(&[p(0.0, 0.0), p(1.0, 0.0)], 0.0, &[], &opts).unwrap_err(), Error::UnsupportedOptions);
    assert_eq!(blob_brush(&[p(f64::NAN, 0.0)], 4.0, &[], &opts).unwrap_err(), Error::InvalidGeometry);
}
