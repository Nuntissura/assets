//! Lane SMB-ILLUSTRATOR-PEN slice 2: parametric shape geometry (STU-VEC-127) against hand values.
use hsk_studio_nib::anchor_path::{AnchorError, AnchorPath, EditAnchor};
use hsk_studio_nib::shape::{CornerKind, EllipseSpec, RectSpec, ShapeError, ellipse, kappa, line, polygon, rectangle, sin_cos_deg, star};
use hsk_studio_nib::{Mirroring, PathPoint};

fn p(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}

fn positions(path: &AnchorPath, contour: usize) -> Vec<PathPoint> {
    path.contours[contour].anchors.iter().map(|a| a.position).collect()
}

fn near(a: PathPoint, b: PathPoint) -> bool {
    (a.x - b.x).abs() < 1e-12 && (a.y - b.y).abs() < 1e-12
}

#[test]
fn rectangles_plain_rounded_clamped_and_corner_kinds() {
    let (plain, applied) = rectangle(&RectSpec::plain(10.0, 20.0, 30.0, 40.0)).unwrap();
    assert_eq!(applied, [0.0; 4]);
    assert_eq!(positions(&plain, 0), vec![p(10.0, 20.0), p(40.0, 20.0), p(40.0, 60.0), p(10.0, 60.0)]);
    assert!(plain.contours[0].closed);
    // STU-VEC-066 clamp: min(requested, shorter edge / 2) = min(r, 2) on a 10 x 4 rectangle, reported.
    let spec = RectSpec { corner_radius: [1.0, 5.0, 0.0, 3.0], corner_kind: CornerKind::Round, ..RectSpec::plain(0.0, 0.0, 10.0, 4.0) };
    let (rounded, applied) = rectangle(&spec).unwrap();
    assert_eq!(applied, [1.0, 2.0, 0.0, 2.0]);
    // top-left r=1: (0,1) -> (1,0); top-right r=2: (8,0) -> (10,2); bottom-right sharp; bottom-left r=2.
    assert_eq!(
        positions(&rounded, 0),
        vec![p(0.0, 1.0), p(1.0, 0.0), p(8.0, 0.0), p(10.0, 2.0), p(10.0, 4.0), p(2.0, 4.0), p(0.0, 2.0)]
    );
    let k = kappa();
    let a = &rounded.contours[0].anchors;
    // Quarter-circle handles point at the corner, length r * kappa.
    assert_eq!((a[0].outgoing, a[1].incoming), (p(0.0, -k), p(-k, 0.0)));
    assert_eq!((a[2].outgoing, a[3].incoming), (p(2.0 * k, 0.0), p(0.0, -2.0 * k)));
    // Inverted rounds bulge inward; bevel = 2 anchors per corner, inset = 3.
    let inverted = rectangle(&RectSpec { corner_kind: CornerKind::RoundInverted, corner_radius: [1.0; 4], ..spec }).unwrap().0;
    assert_eq!(inverted.contours[0].anchors[0].outgoing, p(k, 0.0));
    let bevel = rectangle(&RectSpec { corner_kind: CornerKind::Bevel, corner_radius: [1.0; 4], ..spec }).unwrap().0;
    assert_eq!(bevel.contours[0].anchors.len(), 8);
    let inset = rectangle(&RectSpec { corner_kind: CornerKind::Inset, corner_radius: [1.0; 4], ..spec }).unwrap().0;
    assert_eq!(positions(&inset, 0)[..3], [p(0.0, 1.0), p(1.0, 1.0), p(1.0, 0.0)]);
    // corner_kind none ignores radii; the captured values are the STU-VEC-112 strings.
    let none = rectangle(&RectSpec { corner_kind: CornerKind::None, ..spec }).unwrap();
    assert_eq!((none.0.contours[0].anchors.len(), none.1), (4, [0.0; 4]));
    assert_eq!(CornerKind::from_capture_value("inverse rounded corner"), Some(CornerKind::RoundInverted));
    // The owned form is valid and closed.
    assert!(rounded.to_owned_path().unwrap().is_closed());
}

#[test]
fn ellipse_family_full_ring_pie_and_segment() {
    let r = 10.0;
    let k = kappa() * r;
    let circle = ellipse(&EllipseSpec::full(0.0, 0.0, r, r)).unwrap();
    assert_eq!(positions(&circle, 0), vec![p(10.0, 0.0), p(0.0, 10.0), p(-10.0, 0.0), p(0.0, -10.0)]);
    let a0 = circle.contours[0].anchors[0];
    assert_eq!(a0.mirroring, Mirroring::AngleAndLength);
    // 4/3 tan(22.5 deg) equals kappa to the last bits.
    assert!(near(a0.outgoing, p(0.0, k)) && near(a0.incoming, p(0.0, -k)), "{a0:?}");
    // Ring: outer plus reversed inner contour (non-zero hole).
    let ring = ellipse(&EllipseSpec { inner_ratio: 0.5, ..EllipseSpec::full(0.0, 0.0, r, r) }).unwrap();
    assert_eq!(ring.contours.len(), 2);
    assert_eq!(positions(&ring, 1), vec![p(5.0, 0.0), p(0.0, -5.0), p(-5.0, 0.0), p(0.0, 5.0)]);
    // Pie 0..90: arc (10,0) -> (0,10) then the centre.
    let pie = ellipse(&EllipseSpec { end_angle: 90.0, ..EllipseSpec::full(0.0, 0.0, r, r) }).unwrap();
    assert_eq!(positions(&pie, 0), vec![p(10.0, 0.0), p(0.0, 10.0), p(0.0, 0.0)]);
    assert!(near(pie.contours[0].anchors[0].outgoing, p(0.0, k)));
    // Segment 0..180 with inner 0.5: outer half (3 anchors) then inner half back (3 anchors).
    let seg = ellipse(&EllipseSpec { end_angle: 180.0, inner_ratio: 0.5, ..EllipseSpec::full(0.0, 0.0, r, r) }).unwrap();
    assert_eq!(
        positions(&seg, 0),
        vec![p(10.0, 0.0), p(0.0, 10.0), p(-10.0, 0.0), p(-5.0, 0.0), p(0.0, 5.0), p(5.0, 0.0)]
    );
    // A 45 degree arc is one cubic whose midpoint lies on the circle within 1e-4 r (cubic error).
    let arc = ellipse(&EllipseSpec { end_angle: 45.0, ..EllipseSpec::full(0.0, 0.0, r, r) }).unwrap();
    let mid = hsk_studio_nib::anchor_path::point_at(&arc, 0, 0, 0.5).unwrap();
    assert!(((mid.x * mid.x + mid.y * mid.y).sqrt() - r).abs() < 1e-4 * r, "{mid:?}");
    // An ellipse keeps its radii on the axes.
    let e = ellipse(&EllipseSpec::full(5.0, 5.0, 4.0, 2.0)).unwrap();
    assert_eq!(positions(&e, 0), vec![p(9.0, 5.0), p(5.0, 7.0), p(1.0, 5.0), p(5.0, 3.0)]);
}

#[test]
fn polygon_star_line_and_deterministic_trig() {
    // Exact at multiples of 90 degrees; within 2 ulp-ish of std elsewhere.
    assert_eq!(sin_cos_deg(0.0), (0.0, 1.0));
    assert_eq!(sin_cos_deg(90.0), (1.0, 0.0));
    assert_eq!(sin_cos_deg(-90.0), (-1.0, 0.0));
    assert_eq!(sin_cos_deg(540.0), (0.0, -1.0));
    for d in [1.0, 30.0, 45.0, 60.0, 123.4, -200.0, 359.9] {
        let (s, c) = sin_cos_deg(d);
        let r = f64::to_radians(d);
        assert!((s - r.sin()).abs() < 1e-15 && (c - r.cos()).abs() < 1e-15, "{d}");
    }
    // Square as a 4-gon of radius 10 (first vertex at the top).
    let sq = polygon(0.0, 0.0, 10.0, 4, 0.0).unwrap();
    assert_eq!(positions(&sq, 0), vec![p(0.0, -10.0), p(10.0, 0.0), p(0.0, 10.0), p(-10.0, 0.0)]);
    // Inset 50 on the square adds 4 inner vertices at 10 cos(45) / 2.
    let inset = polygon(0.0, 0.0, 10.0, 4, 50.0).unwrap();
    assert_eq!(inset.contours[0].anchors.len(), 8);
    let v = inset.contours[0].anchors[1].position;
    let d = 10.0 * 0.5_f64.sqrt() / 2.0 / 2.0_f64.sqrt();
    assert!(near(v, p(d, -d)), "{v:?}");
    // Five-point star, inner ratio 0.5: alternating radii 10 and 5.
    let st = star(0.0, 0.0, 10.0, 5, 0.5).unwrap();
    assert_eq!(st.contours[0].anchors.len(), 10);
    for (i, a) in st.contours[0].anchors.iter().enumerate() {
        let r = (a.position.x * a.position.x + a.position.y * a.position.y).sqrt();
        assert!((r - if i % 2 == 0 { 10.0 } else { 5.0 }).abs() < 1e-12, "{i} {r}");
    }
    assert_eq!(st.contours[0].anchors[0].position, p(0.0, -10.0));
    // Line: open, two anchors, exact at 90 degrees.
    let l = line(1.0, 2.0, 5.0, 90.0).unwrap();
    assert_eq!(positions(&l, 0), vec![p(1.0, 2.0), p(1.0, 7.0)]);
    assert!(!l.contours[0].closed);
    assert_eq!(l.contours[0].anchors[1], EditAnchor::corner(1.0, 7.0));
}

#[test]
fn hostile_shape_parameters_name_the_parameter() {
    let refused = |r: Result<AnchorPath, ShapeError>| r.expect_err("refused");
    let e = |parameter: &'static str, error: AnchorError| ShapeError { parameter, error };
    assert_eq!(rectangle(&RectSpec::plain(f64::NAN, 0.0, 1.0, 1.0)).unwrap_err(), e("x", AnchorError::NonFinite));
    assert_eq!(rectangle(&RectSpec::plain(0.0, 0.0, 16348.5, 1.0)).unwrap_err(), e("width", AnchorError::InvalidParameter));
    assert_eq!(rectangle(&RectSpec::plain(0.0, 0.0, 1.0, -1.0)).unwrap_err(), e("height", AnchorError::InvalidParameter));
    let neg = RectSpec { corner_radius: [-1.0, 0.0, 0.0, 0.0], corner_kind: CornerKind::Round, ..RectSpec::plain(0.0, 0.0, 4.0, 4.0) };
    assert_eq!(rectangle(&neg).unwrap_err(), e("corner_radius", AnchorError::InvalidParameter));
    assert_eq!(rectangle(&RectSpec { corner_kind: CornerKind::Fancy, ..neg }).unwrap_err(), e("corner_kind", AnchorError::InvalidParameter));
    assert_eq!(rectangle(&RectSpec { corner_smoothing: 10.0, corner_radius: [0.0; 4], ..neg }).unwrap_err(), e("corner_smoothing", AnchorError::InvalidParameter));
    assert_eq!(refused(polygon(0.0, 0.0, 1.0, 2, 0.0)), e("side_count", AnchorError::InvalidParameter));
    assert_eq!(refused(polygon(0.0, 0.0, 1.0, 101, 0.0)), e("side_count", AnchorError::InvalidParameter));
    assert!(polygon(0.0, 0.0, 1.0, 100, 0.0).is_ok());
    assert_eq!(refused(polygon(0.0, 0.0, 1.0, 6, 100.5)), e("inset_percentage", AnchorError::InvalidParameter));
    assert_eq!(refused(polygon(0.0, 0.0, f64::INFINITY, 6, 0.0)), e("radius", AnchorError::NonFinite));
    assert_eq!(refused(star(0.0, 0.0, 1.0, 51, 0.5)), e("point_count", AnchorError::InvalidParameter));
    assert_eq!(refused(star(0.0, 0.0, 1.0, 5, 0.0)), e("inner_radius", AnchorError::InvalidParameter));
    assert_eq!(refused(ellipse(&EllipseSpec { inner_ratio: 1.0, ..EllipseSpec::full(0.0, 0.0, 1.0, 1.0) })), e("inner_ratio", AnchorError::InvalidParameter));
    assert_eq!(refused(ellipse(&EllipseSpec { end_angle: f64::NAN, ..EllipseSpec::full(0.0, 0.0, 1.0, 1.0) })), e("end_angle", AnchorError::NonFinite));
    assert_eq!(refused(ellipse(&EllipseSpec::full(0.0, 0.0, 2e9, 1.0))), e("rx", AnchorError::OutOfRange));
    assert_eq!(refused(line(0.0, 0.0, -1.0, 0.0)), e("length", AnchorError::InvalidParameter));
    // Geometry beyond the coordinate limit after construction is refused by path validation.
    assert_eq!(refused(line(9.9e8, 0.0, 9.9e8, 0.0)), e("geometry", AnchorError::OutOfRange));
}
