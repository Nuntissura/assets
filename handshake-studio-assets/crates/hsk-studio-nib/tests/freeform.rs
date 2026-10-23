//! Lane SMB-ILLUSTRATOR-PEN-2 slice 3: curvature smoothing, pencil fitting, live corners, knife,
//! shape builder, arc segment, grids and spiral against hand-built geometry.
use hsk_studio_nib::anchor_path::{AnchorAddr, AnchorError, AnchorPath, Contour, EditAnchor};
use hsk_studio_nib::boolean::BooleanOptions;
use hsk_studio_nib::freeform::{CornerTreatment, MAX_SAMPLES, apply_corner, fit_freehand, knife, polygon_contains, shape_builder, smooth_curvature};
use hsk_studio_nib::shape::{RectSpec, arc_segment, kappa, polar_grid, rectangle, rectangular_grid, spiral};
use hsk_studio_nib::{Error, FillRule, Mirroring, OwnedPath, PathElement as E, PathPoint};

fn p(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}

fn near(a: PathPoint, b: PathPoint, tol: f64) -> bool {
    (a.x - b.x).abs() <= tol && (a.y - b.y).abs() <= tol
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

fn square(x: f64, y: f64, s: f64) -> AnchorPath {
    rectangle(&RectSpec::plain(x, y, s, s)).unwrap().0
}

#[test]
fn curvature_smoothing_and_pencil_fit() {
    let smooth = |x: f64, y: f64| EditAnchor { mirroring: Mirroring::AngleAndLength, ..EditAnchor::corner(x, y) };
    let path = AnchorPath { contours: vec![Contour { anchors: vec![smooth(0.0, 0.0), smooth(10.0, 10.0), smooth(20.0, 0.0), EditAnchor::corner(30.0, 10.0), smooth(40.0, 0.0)], closed: false }], fill_rule: FillRule::NonZero };
    let s = smooth_curvature(&path, 0).unwrap();
    let a = &s.contours[0].anchors;
    // Catmull-Rom: (next - prev) / 6, mirrored; the corner and the open ends keep no handles.
    assert!(near(a[1].outgoing, p(20.0 / 6.0, 0.0), 1e-12) && near(a[1].incoming, p(-20.0 / 6.0, 0.0), 1e-12));
    assert!(near(a[2].outgoing, p(20.0 / 6.0, 0.0), 1e-12));
    assert_eq!((a[0].outgoing, a[3].outgoing, a[3].incoming, a[4].incoming), (p(0.0, 0.0), p(0.0, 0.0), p(0.0, 0.0), p(0.0, 0.0)));
    // The curve passes through every placed point (anchors are the points).
    assert_eq!(a.iter().map(|a| a.position).collect::<Vec<_>>(), path.contours[0].anchors.iter().map(|a| a.position).collect::<Vec<_>>());
    // Pencil: a jittered straight stroke reduces to its two ends.
    let line: Vec<PathPoint> = (0..=100).map(|i| p(f64::from(i), if i % 2 == 0 { 0.1 } else { -0.1 })).collect();
    let fitted = fit_freehand(&line, 0.5, None).unwrap();
    assert_eq!(fitted.contours[0].anchors.len(), 2);
    // A sampled circle closes and keeps a handful of anchors, each a sample on the circle.
    let circle: Vec<PathPoint> = (0..=64).map(|i| {
        let t = f64::from(i) / 64.0 * std::f64::consts::TAU;
        p(20.0 * t.cos(), 20.0 * t.sin())
    }).collect();
    let fitted = fit_freehand(&circle, 0.5, Some(1.0)).unwrap();
    let c = &fitted.contours[0];
    assert!(c.closed && (6..=40).contains(&c.anchors.len()), "{}", c.anchors.len());
    for a in &c.anchors {
        assert!(((a.position.x.powi(2) + a.position.y.powi(2)).sqrt() - 20.0).abs() < 1e-9);
        assert_eq!(a.mirroring, Mirroring::AngleAndLength);
    }
    // Hostile input is typed.
    assert_eq!(fit_freehand(&[p(f64::NAN, 0.0), p(1.0, 1.0)], 1.0, None), Err(AnchorError::NonFinite));
    assert_eq!(fit_freehand(&[p(0.0, 0.0), p(1.0, 1.0)], 0.0, None), Err(AnchorError::InvalidParameter));
    assert_eq!(fit_freehand(&[p(1.0, 1.0); 3], 1.0, None), Err(AnchorError::Degenerate));
    assert_eq!(fit_freehand(&vec![p(0.0, 0.0); MAX_SAMPLES + 1], 1.0, None), Err(AnchorError::BudgetExceeded));
}

#[test]
fn live_corners_round_inverted_chamfer_and_clamp() {
    let sq = square(0.0, 0.0, 10.0);
    let at = AnchorAddr { contour: 0, anchor: 1 }; // (10, 0): arriving along +x, leaving along +y
    let k = kappa();
    let (round, r) = apply_corner(&sq, at, CornerTreatment::Round, 3.0).unwrap();
    assert_eq!(r, 3.0);
    let a = &round.contours[0].anchors;
    assert_eq!(a.len(), 5);
    assert!(near(a[1].position, p(7.0, 0.0), 1e-12) && near(a[2].position, p(10.0, 3.0), 1e-12));
    assert!(near(a[1].outgoing, p(3.0 * k, 0.0), 1e-12) && near(a[2].incoming, p(0.0, -3.0 * k), 1e-12));
    let (inv, _) = apply_corner(&sq, at, CornerTreatment::InvertedRound, 3.0).unwrap();
    let a = &inv.contours[0].anchors;
    assert!(near(a[1].outgoing, p(0.0, 3.0 * k), 1e-12) && near(a[2].incoming, p(-3.0 * k, 0.0), 1e-12), "concave: handles point into the shape");
    let (ch, _) = apply_corner(&sq, at, CornerTreatment::Chamfer, 3.0).unwrap();
    assert_eq!((ch.contours[0].anchors[1].outgoing, ch.contours[0].anchors[2].incoming), (p(0.0, 0.0), p(0.0, 0.0)));
    // STU-VEC-066: the radius clamps to half the shorter incident edge and is reported.
    assert_eq!(apply_corner(&sq, at, CornerTreatment::Round, 50.0).unwrap().1, 5.0);
    // A 60 degree corner (equilateral triangle): the round arc stays on the circle tangent to both edges.
    let h = 10.0 * 3.0_f64.sqrt() / 2.0;
    let tri = AnchorPath { contours: vec![Contour { anchors: vec![EditAnchor::corner(0.0, 0.0), EditAnchor::corner(10.0, 0.0), EditAnchor::corner(5.0, h)], closed: true }], fill_rule: FillRule::NonZero };
    let (t, r) = apply_corner(&tri, at, CornerTreatment::Round, 2.0).unwrap();
    // Turning angle 120 degrees: R = r / tan(60) ; centre on the bisector at distance R from both edges.
    let big_r = r / 3.0_f64.sqrt();
    let centre = p(8.0, big_r);
    let mid = hsk_studio_nib::anchor_path::point_at(&t, 0, 1, 0.5).unwrap();
    assert!((((mid.x - centre.x).powi(2) + (mid.y - centre.y).powi(2)).sqrt() - big_r).abs() < 5e-3 * big_r, "{mid:?}");
    // Refusals: an open end, a straight-through anchor, a bad radius.
    let open = AnchorPath { contours: vec![Contour { anchors: vec![EditAnchor::corner(0.0, 0.0), EditAnchor::corner(5.0, 0.0), EditAnchor::corner(10.0, 0.0)], closed: false }], fill_rule: FillRule::NonZero };
    assert_eq!(apply_corner(&open, AnchorAddr { contour: 0, anchor: 0 }, CornerTreatment::Round, 1.0).unwrap_err(), AnchorError::InvalidParameter);
    assert_eq!(apply_corner(&open, AnchorAddr { contour: 0, anchor: 1 }, CornerTreatment::Round, 1.0).unwrap_err(), AnchorError::InvalidParameter);
    assert_eq!(apply_corner(&sq, at, CornerTreatment::Round, f64::NAN).unwrap_err(), AnchorError::NonFinite);
    assert_eq!(apply_corner(&sq, at, CornerTreatment::Round, -1.0).unwrap_err(), AnchorError::InvalidParameter);
    assert_eq!(apply_corner(&sq, AnchorAddr { contour: 0, anchor: 9 }, CornerTreatment::Round, 1.0).unwrap_err(), AnchorError::PointIndexOutOfBounds);
}

#[test]
fn knife_cuts_filled_regions_apart() {
    let opts = BooleanOptions::default();
    let sq = square(0.0, 0.0, 10.0).to_owned_path().unwrap();
    let kerf = 0.01;
    let pieces = knife(&sq, &[p(5.0, -1.0), p(5.0, 11.0)], kerf, &opts).unwrap();
    assert_eq!(pieces.len(), 2);
    for piece in &pieces {
        assert!((area(piece).abs() - (50.0 - 10.0 * kerf / 2.0)).abs() < 1e-6, "{}", area(piece));
    }
    // A diagonal polyline cut through a circle also gives two closed regions.
    let circle = hsk_studio_nib::shape::ellipse(&hsk_studio_nib::shape::EllipseSpec::full(0.0, 0.0, 10.0, 10.0)).unwrap().to_owned_path().unwrap();
    assert_eq!(knife(&circle, &[p(-12.0, -12.0), p(0.0, 1.0), p(12.0, 12.0)], kerf, &opts).unwrap().len(), 2);
    // A cut that does not cross leaves one region (a slit); a cut outside removes nothing.
    assert_eq!(knife(&sq, &[p(5.0, -1.0), p(5.0, 5.0)], kerf, &opts).unwrap().len(), 1);
    let whole = knife(&sq, &[p(20.0, 0.0), p(20.0, 10.0)], kerf, &opts).unwrap();
    assert_eq!((whole.len(), area(&whole[0]).abs()), (1, 100.0));
    // Typed refusals.
    assert_eq!(knife(&sq, &[p(0.0, 0.0)], kerf, &opts).unwrap_err(), Error::InvalidRequest);
    assert_eq!(knife(&sq, &[p(0.0, 0.0), p(f64::INFINITY, 1.0)], kerf, &opts).unwrap_err(), Error::InvalidGeometry);
    assert_eq!(knife(&sq, &[p(0.0, 0.0), p(1.0, 1.0)], 0.0, &opts).unwrap_err(), Error::UnsupportedOptions);
    assert_eq!(knife(&sq, &vec![p(0.0, 0.0); MAX_SAMPLES + 1], kerf, &opts).unwrap_err(), Error::BudgetExceeded);
}

#[test]
fn shape_builder_merges_and_deletes_faces() {
    let opts = BooleanOptions::default();
    let ops = [square(0.0, 0.0, 10.0).to_owned_path().unwrap(), square(5.0, 5.0, 10.0).to_owned_path().unwrap()];
    // A drag across all three faces merges them: the union (175).
    let all = shape_builder(&ops, &[p(2.0, 2.0), p(12.0, 12.0)], false, &opts).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!((area(&all[0].path), all[0].sources.clone()), (175.0, vec![0, 1]));
    // A click in the overlap keeps three faces; deleting it leaves the two L shapes.
    let one = shape_builder(&ops, &[p(7.0, 7.0)], false, &opts).unwrap();
    assert_eq!(one.len(), 3);
    let del = shape_builder(&ops, &[p(7.0, 7.0)], true, &opts).unwrap();
    let mut areas: Vec<f64> = del.iter().map(|f| area(&f.path).abs()).collect();
    areas.sort_by(f64::total_cmp);
    assert_eq!(areas, vec![75.0, 75.0]);
    assert!(del.iter().all(|f| !polygon_contains(&f.path, p(7.0, 7.0))));
    // Merging two faces only: the overlap and the lower-left L (a drag from (2,2) to (7,7)).
    let two = shape_builder(&ops, &[p(2.0, 2.0), p(7.0, 7.0)], false, &opts).unwrap();
    assert_eq!(two.len(), 2);
    assert!(two.iter().any(|f| area(&f.path).abs() == 100.0));
    // Typed refusals: nothing touched, everything deleted, no gesture.
    assert_eq!(shape_builder(&ops, &[p(50.0, 50.0)], false, &opts).unwrap_err(), Error::InvalidRequest);
    assert_eq!(shape_builder(&ops, &[p(2.0, 2.0), p(12.0, 12.0)], true, &opts).unwrap_err(), Error::EmptyPathfinderResult);
    assert_eq!(shape_builder(&ops, &[], false, &opts).unwrap_err(), Error::InvalidRequest);
}

#[test]
fn arc_segment_grids_and_spiral() {
    let k = kappa();
    let arc = arc_segment(0.0, 0.0, 10.0, 20.0, 0.0, false, true).unwrap();
    let a = &arc.contours[0].anchors;
    assert_eq!((a.len(), arc.contours[0].closed), (2, false));
    assert!(near(a[0].outgoing, p(10.0 * k, 0.0), 1e-12) && near(a[1].incoming, p(0.0, -20.0 * k), 1e-12));
    let pie = arc_segment(0.0, 0.0, 10.0, 20.0, -100.0, true, false).unwrap();
    assert_eq!(pie.contours[0].anchors[2].position, p(0.0, 20.0));
    assert_eq!(pie.contours[0].anchors[0].outgoing, p(0.0, 0.0), "slope -100 is straight");
    // Rectangular grid: frame + 2 row + 3 column dividers; even at skew 0, bunched at skew 100.
    let g = rectangular_grid(0.0, 0.0, 30.0, 30.0, 2, 3, 0.0, 0.0, true).unwrap();
    assert_eq!(g.contours.len(), 6);
    assert_eq!(g.contours[1].anchors[0].position, p(0.0, 10.0));
    assert_eq!(g.contours[3].anchors[0].position, p(7.5, 0.0));
    let s = rectangular_grid(0.0, 0.0, 30.0, 30.0, 2, 0, 100.0, 0.0, false).unwrap();
    assert!((s.contours[0].anchors[0].position.y - 6.0).abs() < 1e-12, "t = 1/3 skewed by r = 2: 0.2");
    // Polar grid: outer + 1 concentric ellipse, 4 radial dividers at exact quarter angles.
    let pg = polar_grid(0.0, 0.0, 10.0, 10.0, 1, 4, 0.0, 0.0).unwrap();
    assert_eq!(pg.contours.len(), 6);
    assert_eq!(pg.contours[1].anchors[0].position, p(5.0, 0.0));
    let ends: Vec<PathPoint> = pg.contours[2..].iter().map(|c| c.anchors[1].position).collect();
    assert_eq!(ends, vec![p(10.0, 0.0), p(0.0, 10.0), p(-10.0, 0.0), p(0.0, -10.0)]);
    // Spiral: one turn at decay 50 is four quarter arcs; the radius shrinks by 0.5^(1/4) per quarter.
    let sp = spiral(0.0, 0.0, 16.0, 50.0, 1.0, true).unwrap();
    let a = &sp.contours[0].anchors;
    assert_eq!(a.len(), 5);
    let r1 = 16.0 * 0.5_f64.sqrt().sqrt();
    assert!(near(a[0].position, p(16.0, 0.0), 1e-12) && near(a[1].position, p(0.0, 16.0), 1e-12));
    assert!(near(a[2].position, p(-r1, 16.0 - r1), 1e-9), "{:?}", a[2].position);
    // Tangent continuity: each interior anchor's handles are collinear (ANGLE mirroring).
    for x in &a[1..4] {
        assert_eq!(x.mirroring, Mirroring::Angle);
        assert!((x.incoming.x * x.outgoing.y - x.incoming.y * x.outgoing.x).abs() < 1e-9);
    }
    // Refusals name the parameter.
    assert_eq!(spiral(0.0, 0.0, 1.0, 100.0, 1.0, true).unwrap_err().parameter, "decay");
    assert_eq!(spiral(0.0, 0.0, 1.0, 50.0, 0.0, true).unwrap_err().parameter, "turns");
    assert_eq!(rectangular_grid(0.0, 0.0, 1.0, 1.0, 1001, 0, 0.0, 0.0, true).unwrap_err().parameter, "horizontal_dividers");
    assert_eq!(arc_segment(0.0, 0.0, 1.0, 1.0, 101.0, false, true).unwrap_err().parameter, "slope");
}
