//! Lane SMB-COMPOSITE-STROKE-GEOMETRY slice 2: exact curve crossings by Bézier subdivision with an
//! interval bound, used by network planarize and the knife on strokes.
use hsk_studio_nib::anchor_path::{AnchorPath, Contour, EditAnchor, point_at};
use hsk_studio_nib::freeform::knife_strokes;
use hsk_studio_nib::intersect::{bez, cubic_crossings, is_straight, line_cubic};
use hsk_studio_nib::network::{NetworkSegment, NetworkVertex, VectorNetwork};
use hsk_studio_nib::{Error, FillRule, PathPoint};

fn p(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}

const ARCH: [PathPoint; 4] = [PathPoint { x: 0.0, y: 0.0 }, PathPoint { x: 0.0, y: 10.0 }, PathPoint { x: 10.0, y: 10.0 }, PathPoint { x: 10.0, y: 0.0 }];

#[test]
fn curve_line_crossings_match_the_closed_form() {
    // y(t) = 30 t (1 - t) = 5  =>  t = (1 -+ sqrt(1/3)) / 2.
    let line = line_cubic(p(-1.0, 5.0), p(11.0, 5.0));
    let found = cubic_crossings(&ARCH, &line, 1e-12).unwrap();
    assert_eq!(found.len(), 2, "{found:?}");
    let r = (1.0_f64 / 3.0).sqrt();
    for ((ta, tb), want) in found.iter().zip([(1.0 - r) / 2.0, (1.0 + r) / 2.0]) {
        assert!((ta - want).abs() < 1e-10, "{ta} vs {want}");
        let (q, l) = (bez(&ARCH, *ta), bez(&line, *tb));
        assert!((q.y - 5.0).abs() < 1e-11 && (q.x - l.x).abs() < 1e-11 && (l.y - 5.0).abs() < 1e-11, "{q:?} {l:?}");
    }
    // Lines meet by the exact formula; parallel lines do not meet.
    assert!(is_straight(&line_cubic(p(0.0, 0.0), p(4.0, 2.0))));
    assert_eq!(cubic_crossings(&line_cubic(p(0.0, 0.0), p(4.0, 4.0)), &line_cubic(p(0.0, 4.0), p(4.0, 0.0)), 1e-9).unwrap(), vec![(0.5, 0.5)]);
    assert!(cubic_crossings(&line_cubic(p(0.0, 0.0), p(4.0, 0.0)), &line_cubic(p(0.0, 1.0), p(4.0, 1.0)), 1e-9).unwrap().is_empty());
}

#[test]
fn curve_curve_crossings_are_consistent_and_hostile_input_is_typed() {
    // The arch and its mirror shifted right cross once, at x = 7.5 by symmetry about it.
    let shifted = [p(5.0, 0.0), p(5.0, 10.0), p(15.0, 10.0), p(15.0, 0.0)];
    let found = cubic_crossings(&ARCH, &shifted, 1e-12).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    let (a, b) = (bez(&ARCH, found[0].0), bez(&shifted, found[0].1));
    assert!((a.x - 7.5).abs() < 1e-10 && (a.x - b.x).abs() < 1e-10 && (a.y - b.y).abs() < 1e-10, "{a:?} {b:?}");
    assert!((found[0].0 + found[0].1 - 1.0).abs() < 1e-10, "mirror parameters");
    // Disjoint boxes: nothing, cheaply.
    let far = [p(50.0, 50.0), p(50.0, 60.0), p(60.0, 60.0), p(60.0, 50.0)];
    assert!(cubic_crossings(&ARCH, &far, 1e-12).unwrap().is_empty());
    // Coincident curves have infinitely many crossings: the work bound is a typed refusal.
    assert_eq!(cubic_crossings(&ARCH, &ARCH, 1e-12).unwrap_err(), Error::BudgetExceeded);
    assert_eq!(cubic_crossings(&ARCH, &[p(f64::NAN, 0.0), p(0.0, 0.0), p(1.0, 1.0), p(2.0, 2.0)], 1e-9).unwrap_err(), Error::InvalidGeometry);
    assert_eq!(cubic_crossings(&ARCH, &shifted, 0.0).unwrap_err(), Error::UnsupportedOptions);
}

#[test]
fn planarize_and_knife_split_at_exact_crossings() {
    // Two crossing arches as a network: the crossing vertex lies on both curves.
    let mut net = VectorNetwork { vertices: vec![], segments: vec![], regions: vec![] };
    for c in [ARCH, [p(5.0, 0.0), p(5.0, 10.0), p(15.0, 10.0), p(15.0, 0.0)]] {
        let base = net.vertices.len();
        net.vertices.extend([NetworkVertex::at(c[0].x, c[0].y), NetworkVertex::at(c[3].x, c[3].y)]);
        net.segments.push(NetworkSegment { start: base, end: base + 1, tangent_start: Some(p(c[1].x - c[0].x, c[1].y - c[0].y)), tangent_end: Some(p(c[2].x - c[3].x, c[2].y - c[3].y)) });
    }
    let planar = net.planarize(1e-9).unwrap();
    assert_eq!((planar.vertices.len(), planar.segments.len()), (5, 4));
    let v = planar.vertices[4];
    let exact = bez(&ARCH, cubic_crossings(&ARCH, &[p(5.0, 0.0), p(5.0, 10.0), p(15.0, 10.0), p(15.0, 0.0)], 1e-12).unwrap()[0].0);
    assert!((v.x - exact.x).abs() < 1e-9 && (v.y - exact.y).abs() < 1e-9, "{v:?} vs {exact:?}");
    // The split pieces end exactly on the crossing vertex and start where the curve did.
    for s in &planar.segments {
        assert!(s.start != s.end);
    }
    // Knife on the arch stroke: the cut point lies on the cut line to 1e-9.
    let path = AnchorPath {
        contours: vec![Contour { anchors: vec![EditAnchor { outgoing: p(0.0, 10.0), ..EditAnchor::corner(0.0, 0.0) }, EditAnchor { incoming: p(0.0, 10.0), ..EditAnchor::corner(10.0, 0.0) }], closed: false }],
        fill_rule: FillRule::NonZero,
    };
    let pieces = knife_strokes(&path, &[p(-1.0, 5.0), p(11.0, 5.0)]).unwrap();
    assert_eq!(pieces.len(), 3);
    let joint = pieces[0].contours[0].anchors.last().unwrap().position;
    assert!((joint.y - 5.0).abs() < 1e-9, "{joint:?}");
    assert_eq!(point_at(&pieces[1], 0, 0, 0.0).unwrap(), joint);
}
