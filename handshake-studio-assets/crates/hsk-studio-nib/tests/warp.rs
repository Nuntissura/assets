//! Lane SMB-ILLUSTRATOR-TOOLS-2: envelope distort and the fifteen warp styles (STU-VEC-168,
//! STU-VEC-169). Expected values are hand-derived from the Studio formulas (module docs).
use hsk_studio_nib::arrange::{Bounds, path_bounds, union_bounds};
use hsk_studio_nib::warp::*;
use hsk_studio_nib::{Error, OwnedPath, PathElement, PathPoint};

fn bx(x0: f64, y0: f64, x1: f64, y1: f64) -> Bounds {
    Bounds { min: [x0, y0], max: [x1, y1] }
}
fn close(a: (f64, f64), e: (f64, f64)) {
    assert!((a.0 - e.0).abs() < 1e-9 && (a.1 - e.1).abs() < 1e-9, "{a:?} != {e:?}");
}
fn bounds_close(a: Bounds, e: Bounds, tol: f64) {
    let d = a.min.iter().chain(a.max.iter()).zip(e.min.iter().chain(e.max.iter())).map(|(x, y)| (x - y).abs()).fold(0.0, f64::max);
    assert!(d <= tol, "{a:?} != {e:?}");
}
fn points(p: &OwnedPath) -> Vec<PathPoint> {
    p.elements.iter().filter_map(|e| match e {
        PathElement::MoveTo(q) | PathElement::LineTo(q) => Some(*q),
        _ => None,
    }).collect()
}
fn warp(style: WarpStyle, bend: f64) -> Warp {
    Warp { style, axis: WarpAxis::Horizontal, bend, distortion_horizontal: 0.0, distortion_vertical: 0.0 }
}

#[test]
fn warp_styles_and_envelopes_map_points_as_specified() {
    // Bend 0 is the identity for every style; the art keeps its bounds.
    let art = vec![rect(&bx(0.0, 0.0, 100.0, 50.0))];
    for style in WarpStyle::ALL {
        close(warp_point(&warp(style, 0.0), 0.3, -0.7), (0.3, -0.7));
        let out = envelope_paths(&art, &Envelope::Warp(warp(style, 0.0)), 50.0).unwrap();
        bounds_close(union_bounds(&out).unwrap(), bx(0.0, 0.0, 100.0, 50.0), 1e-9);
    }
    // Hand values in normalised coordinates.
    close(warp_point(&warp(WarpStyle::ArcLower, 0.5), 0.0, 1.0), (0.0, 0.5));
    close(warp_point(&warp(WarpStyle::ArcLower, 0.5), 0.0, -1.0), (0.0, -1.0));
    close(warp_point(&warp(WarpStyle::Bulge, 0.5), 0.0, 1.0), (0.0, 1.5));
    close(warp_point(&warp(WarpStyle::Wave, 0.5), -0.5, 0.0), (-0.5, -0.5));
    let t = warp_point(&warp(WarpStyle::Twist, 0.5), 0.5, 0.0);
    close(t, (0.5 * (std::f64::consts::FRAC_PI_4).cos(), 0.5 * (std::f64::consts::FRAC_PI_4).sin()));
    close(warp_point(&Warp { axis: WarpAxis::Vertical, ..warp(WarpStyle::Arch, 0.25) }, 1.0, 0.0), (0.75, 0.0));
    close(warp_point(&Warp { distortion_horizontal: 0.5, ..warp(WarpStyle::Arc, 0.0) }, 1.0, 1.0), (1.0, 1.5));
    // Bulge 0.5 on the 100 x 50 box: the bottom and top edges reach 1.5 half-heights from the centre.
    let bulged = envelope_paths(&art, &Envelope::Warp(warp(WarpStyle::Bulge, 0.5)), 100.0).unwrap();
    bounds_close(union_bounds(&bulged).unwrap(), bx(0.0, -12.5, 100.0, 62.5), 0.05);
    // Mesh: the plain grid is the identity; dragging the bottom-right point moves that corner.
    let b = bx(0.0, 0.0, 100.0, 50.0);
    let grid = Mesh::grid(&b, 2, 3).unwrap();
    bounds_close(union_bounds(&envelope_paths(&art, &Envelope::Mesh(grid.clone()), 50.0).unwrap()).unwrap(), b, 1e-9);
    let mut dragged = grid.clone();
    *dragged.points.last_mut().unwrap() = PathPoint::new(150.0, 80.0);
    let out = envelope_paths(&art, &Envelope::Mesh(dragged), 50.0).unwrap();
    assert!(points(&out[0]).iter().any(|q| (q.x - 150.0).abs() < 1e-9 && (q.y - 80.0).abs() < 1e-9));
    // Top object: a 200 x 100 rectangle receives the 10 x 10 art exactly.
    let small = vec![rect(&bx(0.0, 0.0, 10.0, 10.0))];
    let top = rect(&bx(20.0, 30.0, 220.0, 130.0));
    let wrapped = envelope_paths(&small, &Envelope::Shape(top), 50.0).unwrap();
    bounds_close(path_bounds(&wrapped[0]).unwrap(), bx(20.0, 30.0, 220.0, 130.0), 1e-6);
    // Hostile input: bend out of range, a short mesh, non-finite fidelity.
    assert_eq!(envelope_paths(&art, &Envelope::Warp(warp(WarpStyle::Arc, 2.0)), 50.0), Err(Error::InvalidRequest));
    assert_eq!(envelope_paths(&art, &Envelope::Mesh(Mesh { rows: 1, columns: 1, points: vec![PathPoint::new(0.0, 0.0)] }), 50.0), Err(Error::InvalidRequest));
    assert_eq!(envelope_paths(&art, &Envelope::Warp(warp(WarpStyle::Arc, 0.1)), f64::NAN), Err(Error::InvalidRequest));
    assert_eq!(Mesh::grid(&b, 0, 3), Err(Error::InvalidRequest));
}
