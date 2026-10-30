//! Lane SMB-AI-SIMPLIFY-1: Simplify (STU-VEC-114), the smooth tool's smoothing (STU-VEC-007) and the
//! OwnedPath entry, against hand-built geometry and an independent distance measure.
use hsk_studio_nib::anchor_path::{AnchorError, AnchorPath, Contour, EditAnchor, insert_anchor};
use hsk_studio_nib::freeform::smooth_curvature;
use hsk_studio_nib::shape::sin_cos_deg;
use hsk_studio_nib::simplify::{SAMPLES_PER_SEGMENT, SMOOTH_TOOL_DEFAULT, SimplifyParams, carry, simplify, simplify_owned, smooth};
use hsk_studio_nib::{FillRule, Mirroring, OwnedPath, PathElement as E, PathPoint};

fn p(x: f64, y: f64) -> PathPoint {
    PathPoint::new(x, y)
}

fn path(contours: Vec<Contour>) -> AnchorPath {
    AnchorPath { contours, fill_rule: FillRule::NonZero }
}

fn polyline(points: &[(f64, f64)], closed: bool) -> Contour {
    Contour { anchors: points.iter().map(|&(x, y)| EditAnchor::corner(x, y)).collect(), closed }
}

/// Smooth anchors through `points`, Catmull-Rom handles (curvature tool).
fn smooth_through(points: &[(f64, f64)], closed: bool) -> Contour {
    let anchors = points.iter().map(|&(x, y)| EditAnchor { mirroring: Mirroring::AngleAndLength, ..EditAnchor::corner(x, y) }).collect();
    smooth_curvature(&path(vec![Contour { anchors, closed }]), 0).unwrap().contours.remove(0)
}

fn circle(n: u32, r: f64) -> Contour {
    let pts: Vec<(f64, f64)> = (0..n).map(|i| sin_cos_deg(360.0 * f64::from(i) / f64::from(n))).map(|(s, c)| (200.0 + r * c, 200.0 + r * s)).collect();
    smooth_through(&pts, true)
}

/// Open polyline of a sampled sine wave (all corner anchors, angles near 180).
fn wobbly() -> Contour {
    let pts: Vec<(f64, f64)> = (0..61).map(|i| (f64::from(i) * 5.0, 20.0 * sin_cos_deg(f64::from(i) * 12.0).0)).collect();
    polyline(&pts, false)
}

/// Open smooth wave.
fn smooth_wave() -> Contour {
    let pts: Vec<(f64, f64)> = (0..25).map(|i| (f64::from(i) * 12.0, 30.0 * sin_cos_deg(f64::from(i) * 25.0).0)).collect();
    smooth_through(&pts, false)
}

/// Zigzag with interior angles of many sizes.
fn zigzag() -> Contour {
    let heights = [2.0, 40.0, 5.0, 25.0, 1.0, 60.0, 10.0, 3.0, 35.0, 0.5, 15.0];
    let pts: Vec<(f64, f64)> = heights.iter().enumerate().map(|(i, h)| (f64::from(u32::try_from(i).unwrap()) * 10.0, if i % 2 == 1 { *h } else { -*h })).collect();
    polyline(&pts, false)
}

fn square() -> Contour {
    polyline(&[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)], true)
}

fn params(precision: f64) -> SimplifyParams {
    SimplifyParams { curve_precision: precision, ..SimplifyParams::default() }
}

fn bez(c: &[PathPoint; 4], t: f64) -> PathPoint {
    let u = 1.0 - t;
    let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    p(w.iter().zip(c).map(|(w, q)| w * q.x).sum(), w.iter().zip(c).map(|(w, q)| w * q.y).sum())
}

/// Anchor 0 and `SAMPLES_PER_SEGMENT` samples per segment of contour `ci`.
fn samples(path: &AnchorPath, ci: usize) -> Vec<PathPoint> {
    let c = &path.contours[ci];
    let mut out = vec![c.anchors[0].position];
    for si in 0..c.segment_count() {
        let ctl = path.segment(ci, si).unwrap();
        for s in 1..=SAMPLES_PER_SEGMENT {
            out.push(bez(&ctl, f64::from(s) / f64::from(SAMPLES_PER_SEGMENT)));
        }
    }
    out
}

fn dist(a: PathPoint, b: PathPoint) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

/// Distance from every point to the curve of contour `ci` of `path`: 128 samples per segment, then a
/// ternary refinement around every sample within one sample step of the best (independent of the
/// implementation's own measure).
fn distances(points: &[PathPoint], path: &AnchorPath, ci: usize) -> Vec<f64> {
    let c = &path.contours[ci];
    let n = 128_u32;
    let segs: Vec<[PathPoint; 4]> = (0..c.segment_count()).map(|si| path.segment(ci, si).unwrap()).collect();
    if segs.is_empty() {
        return points.iter().map(|q| dist(*q, c.anchors[0].position)).collect();
    }
    let grid: Vec<Vec<PathPoint>> = segs.iter().map(|ctl| (0..=n).map(|k| bez(ctl, f64::from(k) / f64::from(n))).collect()).collect();
    let step = grid.iter().flat_map(|g| g.windows(2).map(|w| dist(w[0], w[1]))).fold(0.0, f64::max);
    points
        .iter()
        .map(|q| {
            let ds: Vec<Vec<f64>> = grid.iter().map(|g| g.iter().map(|v| dist(*q, *v)).collect()).collect();
            let mut best = ds.iter().flatten().copied().fold(f64::INFINITY, f64::min);
            let cut = best + step;
            for (si, row) in ds.iter().enumerate() {
                for (k, d) in row.iter().enumerate() {
                    if *d > cut {
                        continue;
                    }
                    let k = f64::from(u32::try_from(k).unwrap());
                    let (mut lo, mut hi) = (((k - 1.0) / f64::from(n)).max(0.0), ((k + 1.0) / f64::from(n)).min(1.0));
                    for _ in 0..80 {
                        let (m1, m2) = (lo + (hi - lo) / 3.0, hi - (hi - lo) / 3.0);
                        if dist(*q, bez(&segs[si], m1)) <= dist(*q, bez(&segs[si], m2)) { hi = m2 } else { lo = m1 }
                    }
                    best = best.min(dist(*q, bez(&segs[si], (lo + hi) / 2.0)));
                }
            }
            best
        })
        .collect()
}

fn shapes() -> Vec<AnchorPath> {
    vec![path(vec![wobbly()]), path(vec![circle(48, 80.0)]), path(vec![smooth_wave()]), path(vec![zigzag()]), path(vec![square(), circle(24, 50.0), wobbly()])]
}

#[test]
fn not_proven_lines() {
    println!("NOT_PROVEN: Illustrator Simplify parity (tolerance mapping, corner rule, refit, smoothing): no D5 oracle case; Studio readings only");
    println!("NOT_PROVEN: window Simplify command (dialog, preview, show original path): no GUI lane yet");
}

#[test]
fn simplify_a_anchor_count_never_increases() {
    for shape in shapes() {
        for precision in [-20.0, 0.0, 50.0, 90.0, 100.0, 150.0] {
            for angle in [0.0, 90.0, 200.0] {
                for lines in [false, true] {
                    for smoothing in [0.0, 50.0] {
                        let prm = SimplifyParams { curve_precision: precision, angle_threshold: angle, smoothing, convert_to_straight_lines: lines, ..SimplifyParams::default() };
                        let (out, rep) = simplify(&shape, &prm).unwrap();
                        assert_eq!(out.contours.len(), shape.contours.len());
                        for (ci, c) in out.contours.iter().enumerate() {
                            assert!(c.anchors.len() <= shape.contours[ci].anchors.len(), "{precision} {angle} {lines} {smoothing}");
                            assert_eq!(rep.contours[ci].anchors_before, shape.contours[ci].anchors.len());
                            assert_eq!(rep.contours[ci].anchors_after, c.anchors.len());
                        }
                        assert!(out.anchor_count() <= shape.anchor_count());
                    }
                }
            }
        }
    }
}

#[test]
fn simplify_b_original_samples_within_reported_tolerance() {
    for shape in shapes() {
        for precision in [0.0, 50.0, 90.0] {
            let (out, rep) = simplify(&shape, &params(precision)).unwrap();
            for ci in 0..shape.contours.len() {
                let r = &rep.contours[ci];
                assert!(r.tolerance > 0.0);
                assert!(r.max_error <= r.tolerance, "max_error {} > tolerance {}", r.max_error, r.tolerance);
                let worst = distances(&samples(&shape, ci), &out, ci).into_iter().fold(0.0, f64::max);
                assert!(worst <= r.tolerance + 1e-6, "precision {precision} contour {ci}: sample {worst} beyond tolerance {}", r.tolerance);
            }
        }
    }
    // The tolerance falls as the precision rises, and precision 0 reduces the dense shapes.
    let w = path(vec![wobbly()]);
    let tol = |pr: f64| simplify(&w, &params(pr)).unwrap().1.contours[0].tolerance;
    assert!(tol(0.0) > tol(50.0) && tol(50.0) > tol(90.0) && tol(90.0) > 0.0);
    for shape in [path(vec![wobbly()]), path(vec![circle(48, 80.0)])] {
        let (out, _) = simplify(&shape, &params(0.0)).unwrap();
        assert!(out.anchor_count() < shape.anchor_count() / 2, "{} of {}", out.anchor_count(), shape.anchor_count());
    }
}

#[test]
fn simplify_c_precision_100_removes_exactly_redundant_anchors() {
    // Nothing redundant: byte-identical.
    let clean = path(vec![circle(4, 50.0), zigzag(), square(), smooth_wave()]);
    let (out, rep) = simplify(&clean, &params(100.0)).unwrap();
    assert_eq!(out, clean);
    assert_eq!(format!("{out:?}"), format!("{clean:?}"));
    assert_eq!(out.to_owned_path().unwrap(), clean.to_owned_path().unwrap());
    assert!(rep.contours.iter().all(|r| r.tolerance == 0.0 && r.max_error == 0.0 && r.anchors_before == r.anchors_after));
    // Planted collinear anchor (50,0), duplicate (100,0) and collinear (10,10) on an open polyline.
    let planted = path(vec![
        polyline(&[(0.0, 0.0), (50.0, 0.0), (100.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)], true),
        polyline(&[(0.0, 0.0), (10.0, 10.0), (20.0, 20.0), (30.0, 0.0)], false),
    ]);
    let (out, rep) = simplify(&planted, &params(100.0)).unwrap();
    assert_eq!(out, path(vec![square(), polyline(&[(0.0, 0.0), (20.0, 20.0), (30.0, 0.0)], false)]));
    assert_eq!((rep.contours[0].anchors_before, rep.contours[0].anchors_after), (6, 4));
    assert_eq!((rep.contours[1].anchors_before, rep.contours[1].anchors_after), (4, 3));
    // A backtracking anchor on a line is not redundant.
    let back = path(vec![polyline(&[(0.0, 0.0), (30.0, 0.0), (10.0, 0.0)], false)]);
    assert_eq!(simplify(&back, &params(100.0)).unwrap().0, back);
    // An anchor that splits one cubic (de Casteljau) is removed; the merged cubic is the original.
    let base = path(vec![circle(4, 50.0)]);
    let split = insert_anchor(&base, 0, 1, 0.37).unwrap();
    assert_eq!(split.anchor_count(), 5);
    let (out, _) = simplify(&split, &params(100.0)).unwrap();
    assert_eq!(out.anchor_count(), 4);
    for (a, b) in out.contours[0].anchors.iter().zip(&base.contours[0].anchors) {
        assert_eq!(a.position, b.position);
        for (h, g) in [(a.incoming, b.incoming), (a.outgoing, b.outgoing)] {
            assert!((h.x - g.x).abs() <= 1e-9 && (h.y - g.y).abs() <= 1e-9, "{h:?} vs {g:?}");
        }
    }
}

#[test]
fn simplify_d_out_of_soft_range_values_are_accepted_and_echoed() {
    let w = path(vec![wobbly()]);
    let (at100, _) = simplify(&w, &params(100.0)).unwrap();
    let (at150, rep150) = simplify(&w, &params(150.0)).unwrap();
    assert_eq!(rep150.curve_precision, 150.0);
    assert_eq!(at150, at100, "above 100 behaves as 100");
    let (_, rep0) = simplify(&w, &params(0.0)).unwrap();
    let (atm20, repm20) = simplify(&w, &params(-20.0)).unwrap();
    assert_eq!(repm20.curve_precision, -20.0);
    assert!(repm20.contours[0].tolerance > rep0.contours[0].tolerance, "below 0 continues the line");
    assert!(atm20.anchor_count() <= w.anchor_count());
    let prm = SimplifyParams { angle_threshold: 200.0, ..SimplifyParams::default() };
    let (at200, rep200) = simplify(&w, &prm).unwrap();
    assert_eq!(rep200.angle_threshold, 200.0);
    assert_eq!(rep200.contours[0].protected, (1..60).collect::<Vec<_>>(), "above 180 every interior anchor is a corner");
    assert_eq!(at200, w);
    let prm = SimplifyParams { angle_threshold: -20.0, smoothing: 200.0, ..SimplifyParams::default() };
    let (_, rep) = simplify(&path(vec![smooth_wave()]), &prm).unwrap();
    assert_eq!((rep.angle_threshold, rep.smoothing), (-20.0, 200.0));
    assert!(rep.contours[0].protected.is_empty());
    // Precision 2 carry.
    assert_eq!(carry(33.336), 33.34);
    assert_eq!(carry(-0.004).to_bits(), 0.0_f64.to_bits());
    assert_eq!(carry(1.0e15), 1.0e15);
    let (_, rep) = simplify(&w, &params(33.336)).unwrap();
    assert_eq!(rep.curve_precision, 33.34);
    // Non-finite parameters are typed refusals.
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(simplify(&w, &params(bad)).unwrap_err(), AnchorError::NonFinite);
        assert_eq!(simplify(&w, &SimplifyParams { angle_threshold: bad, ..SimplifyParams::default() }).unwrap_err(), AnchorError::NonFinite);
        assert_eq!(simplify(&w, &SimplifyParams { smoothing: bad, ..SimplifyParams::default() }).unwrap_err(), AnchorError::NonFinite);
        assert_eq!(smooth(&w, 0, bad).unwrap_err(), AnchorError::NonFinite);
    }
    assert_eq!(smooth(&w, 3, 10.0).unwrap_err(), AnchorError::PathIndexOutOfBounds);
}

#[test]
fn simplify_e_raising_angle_threshold_protects_a_superset() {
    let z = path(vec![zigzag()]);
    let mut previous: Vec<usize> = Vec::new();
    let mut sizes = Vec::new();
    for angle in [0.0, 10.0, 30.0, 60.0, 90.0, 120.0, 150.0, 179.99, 180.0, 180.01, 200.0, 400.0] {
        let prm = SimplifyParams { angle_threshold: angle, ..SimplifyParams::default() };
        let (out, rep) = simplify(&z, &prm).unwrap();
        let now = rep.contours[0].protected.clone();
        assert!(previous.iter().all(|i| now.contains(i)), "{angle}: {previous:?} not within {now:?}");
        for &i in &now {
            assert!(out.contours[0].anchors.contains(&z.contours[0].anchors[i]), "{angle}: corner {i} changed");
        }
        sizes.push(now.len());
        previous = now;
    }
    assert_eq!(sizes.first(), Some(&0));
    assert_eq!(sizes.last(), Some(&9));
    assert!(sizes.windows(2).any(|w| w[0] < w[1] && w[1] < 9), "{sizes:?}");
}

#[test]
fn simplify_f_convert_to_straight_lines_gives_zero_handles() {
    for shape in shapes() {
        for precision in [0.0, 50.0, 100.0] {
            let prm = SimplifyParams { curve_precision: precision, convert_to_straight_lines: true, smoothing: 40.0, angle_threshold: 90.0, ..SimplifyParams::default() };
            let (out, _) = simplify(&shape, &prm).unwrap();
            for a in out.contours.iter().flat_map(|c| &c.anchors) {
                assert_eq!((a.incoming, a.outgoing, a.mirroring), (p(0.0, 0.0), p(0.0, 0.0), Mirroring::None));
            }
        }
    }
}

#[test]
fn simplify_g_display_flags_do_not_change_the_result() {
    for shape in shapes() {
        let base = SimplifyParams { curve_precision: 50.0, angle_threshold: 45.0, smoothing: 30.0, ..SimplifyParams::default() };
        let reference = simplify(&shape, &base).unwrap();
        for (show, preview) in [(false, false), (true, false), (true, true), (false, true)] {
            let r = simplify(&shape, &SimplifyParams { show_original_path: show, preview, ..base }).unwrap();
            assert_eq!(format!("{r:?}"), format!("{reference:?}"));
        }
    }
    assert_eq!(SimplifyParams::default(), SimplifyParams { curve_precision: 0.0, angle_threshold: 0.0, smoothing: 0.0, convert_to_straight_lines: false, show_original_path: false, preview: true });
}

#[test]
fn simplify_h_repeat_runs_are_byte_identical() {
    for shape in shapes() {
        for precision in [-20.0, 0.0, 50.0, 100.0] {
            let prm = SimplifyParams { curve_precision: precision, angle_threshold: 60.0, smoothing: 25.0, ..SimplifyParams::default() };
            let first = format!("{:?}", simplify(&shape, &prm).unwrap());
            for _ in 0..2 {
                assert_eq!(format!("{:?}", simplify(&shape.clone(), &prm).unwrap()), first);
            }
        }
    }
}

#[test]
fn simplify_i_closed_contours_stay_closed_with_the_same_start() {
    let multi = path(vec![square(), circle(24, 50.0), wobbly(), circle(3, 10.0)]);
    for precision in [-20.0, 0.0, 50.0, 100.0] {
        for lines in [false, true] {
            let (out, _) = simplify(&multi, &SimplifyParams { curve_precision: precision, convert_to_straight_lines: lines, ..SimplifyParams::default() }).unwrap();
            assert_eq!(out.contours.len(), multi.contours.len());
            for (a, b) in out.contours.iter().zip(&multi.contours) {
                assert_eq!(a.closed, b.closed);
                assert_eq!(a.anchors[0].position, b.anchors[0].position);
                assert!(!a.closed || a.anchors.len() >= 2);
            }
        }
    }
}

// ---- slice 2: smoothing ---------------------------------------------------------------------------

/// Corner, kinked smooth anchor (in (-10,0), out (5,8)), corner.
fn kinked() -> AnchorPath {
    let mid = EditAnchor { position: p(10.0, 0.0), incoming: p(-10.0, 0.0), outgoing: p(5.0, 8.0), mirroring: Mirroring::Angle };
    path(vec![Contour { anchors: vec![EditAnchor::corner(0.0, 0.0), mid, EditAnchor::corner(20.0, 0.0)], closed: false }])
}

fn discontinuity(a: &EditAnchor) -> f64 {
    let (i, o) = (a.incoming, a.outgoing);
    let cos = -(i.x * o.x + i.y * o.y) / ((i.x * i.x + i.y * i.y).sqrt() * (o.x * o.x + o.y * o.y).sqrt());
    1.0 - cos
}

#[test]
fn smooth_zero_is_identity_and_hundred_is_curvature() {
    let k = kinked();
    assert_eq!(format!("{:?}", smooth(&k, 0, 0.0).unwrap()), format!("{k:?}"));
    let mut wave = path(vec![smooth_wave()]);
    // Disturb the handles and make anchor 5 a corner.
    for (i, a) in wave.contours[0].anchors.iter_mut().enumerate() {
        a.outgoing = p(a.outgoing.x + 1.5, a.outgoing.y - f64::from(u32::try_from(i).unwrap() % 3));
        if i == 5 {
            *a = EditAnchor { mirroring: Mirroring::None, ..*a };
        }
    }
    let full = smooth(&wave, 0, 100.0).unwrap();
    let curv = smooth_curvature(&wave, 0).unwrap();
    for ((s, c), orig) in full.contours[0].anchors.iter().zip(&curv.contours[0].anchors).zip(&wave.contours[0].anchors) {
        if orig.mirroring == Mirroring::None {
            assert_eq!(s, orig, "corners are untouched");
        } else {
            assert_eq!((s.incoming, s.outgoing), (c.incoming, c.outgoing));
        }
    }
    assert_eq!(SMOOTH_TOOL_DEFAULT, 20.0);
    let d = smooth(&k, 0, SMOOTH_TOOL_DEFAULT).unwrap();
    let t = p(20.0 / 6.0, 0.0);
    let o = d.contours[0].anchors[1].outgoing;
    assert!((o.x - (5.0 * 0.8 + t.x * 0.2)).abs() < 1e-12 && (o.y - 8.0 * 0.8).abs() < 1e-12);
}

#[test]
fn smooth_discontinuity_shrinks_monotonically_and_corners_stay() {
    let k = kinked();
    let mut last = f64::INFINITY;
    for s in [0.0, 10.0, 25.0, 50.0, 75.0, 90.0, 100.0] {
        let d = discontinuity(&smooth(&k, 0, s).unwrap().contours[0].anchors[1]);
        assert!(d <= last, "{s}: {d} > {last}");
        last = d;
    }
    assert!(last <= 1e-12);
    assert!(discontinuity(&k.contours[0].anchors[1]) > 0.1);
    for s in [-50.0, 37.5, 250.0] {
        let out = smooth(&k, 0, s).unwrap();
        assert_eq!(out.contours[0].anchors[0], k.contours[0].anchors[0]);
        assert_eq!(out.contours[0].anchors[2], k.contours[0].anchors[2]);
    }
    // Simplify applies smoothing after the fit; protected corners are untouched.
    let z = path(vec![zigzag()]);
    let prm = SimplifyParams { curve_precision: 100.0, angle_threshold: 30.0, smoothing: 100.0, ..SimplifyParams::default() };
    let (out, rep) = simplify(&z, &prm).unwrap();
    assert_eq!(rep.smoothing, 100.0);
    assert!(!rep.contours[0].protected.is_empty() && rep.contours[0].protected.len() < 9);
    for (i, (a, orig)) in out.contours[0].anchors.iter().zip(&z.contours[0].anchors).enumerate() {
        if rep.contours[0].protected.contains(&i) {
            assert_eq!(a, orig);
        } else if i > 0 && i < 10 {
            assert_eq!(a.mirroring, Mirroring::AngleAndLength, "anchor {i}");
        }
    }
}

// ---- slice 3: OwnedPath entry and descriptor -------------------------------------------------------

#[test]
fn simplify_owned_keeps_a_quad_segment_at_precision_100() {
    let (p0, c, p1) = (p(0.0, 0.0), p(50.0, 80.0), p(100.0, 0.0));
    let owned = OwnedPath { elements: vec![E::MoveTo(p0), E::QuadTo(c, p1), E::LineTo(p(60.0, -40.0)), E::Close], fill_rule: FillRule::NonZero };
    let (out, rep) = simplify_owned(&owned, &params(100.0)).unwrap();
    assert_eq!(rep.contours.len(), 1);
    let [E::MoveTo(m), E::CubicTo(c1, c2, e), E::LineTo(l), E::Close] = out.elements[..] else { panic!("{:?}", out.elements) };
    assert_eq!((m, e, l), (p0, p1, p(60.0, -40.0)));
    for s in 0..=64 {
        let t = f64::from(s) / 64.0;
        let u = 1.0 - t;
        let q = p(u * u * p0.x + 2.0 * u * t * c.x + t * t * p1.x, u * u * p0.y + 2.0 * u * t * c.y + t * t * p1.y);
        let b = bez(&[m, c1, c2, e], t);
        assert!((q.x - b.x).abs() <= 1e-9 && (q.y - b.y).abs() <= 1e-9, "t {t}: {q:?} vs {b:?}");
    }
    assert_eq!(simplify_owned(&owned, &params(f64::NAN)).unwrap_err(), AnchorError::NonFinite);
    let d = hsk_studio_nib::descriptor();
    assert!(d.contains("\"simplify\":{\"lane\":\"SMB-AI-SIMPLIFY-1\"") && d.contains("STU-VEC-114") && d.contains("tests/vector_simplify.rs"));
}
