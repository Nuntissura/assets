//! After Effects blur and edge kernels (lane SMB-PIGMENT-AE-KERNELS-1): `blur_math::box_kernel`,
//! `blur_window_axis`, `directional_blur_window`, `radial_blur_window` and
//! `filter_math::find_edges_window`. Goldens are derived by hand from the declared readings in the
//! module docs (After Effects / Photoshop maths UNVERIFIED, D5); properties pin the structure
//! (normalisation, separability, symmetry, what each path preserves, window independence) and the
//! typed refusals (V4-01/02).
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blur_math::{
    BlurAxis, BlurError, BlurLimits, BlurScope, LayerBlur, MAX_BOX_KERNEL_TAPS, RadialBlurKind, blur_window_axis,
    blur_window_axis_cost, blur_window_in, box_kernel, directional_blur_window, directional_blur_window_cost,
    radial_blur_window, radial_blur_window_cost,
};
use hsk_studio_pigment::filter_math::{FilterError, FilterLimits, find_edges_window, find_edges_window_cost};
use hsk_studio_pigment::render_plan::SampleRange;

const WIDE: BlurLimits = BlurLimits { max_bytes: 1 << 30, max_work: 1 << 40 };
const FWIDE: FilterLimits = FilterLimits { max_bytes: 1 << 30, max_work: 1 << 40 };
const UNIT: SampleRange = SampleRange::Unit;

fn rect(x: i64, y: i64, width: u32, height: u32) -> Rect {
    Rect { x, y, width, height }
}

/// A deterministic unit-range plane (an LCG), with some fully transparent pixels.
fn plane(w: u32, h: u32, seed: u64) -> Vec<[f32; 4]> {
    let mut s = seed;
    let mut next = move || {
        s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((s >> 40) as f32) / ((1u64 << 24) as f32)
    };
    (0..w * h)
        .map(|i| {
            let p = [next(), next(), next(), next()];
            if i % 7 == 3 { [p[0], p[1], p[2], 0.0] } else { p }
        })
        .collect()
}

fn dot(at: (i64, i64), px: [f32; 4]) -> (Vec<[f32; 4]>, Rect) {
    (vec![px], rect(at.0, at.1, 1, 1))
}

fn close(a: &[[f32; 4]], b: &[[f32; 4]], tol: f32, what: &str) {
    assert_eq!(a.len(), b.len(), "{what}: length");
    for (i, (p, q)) in a.iter().zip(b).enumerate() {
        for (c, (u, v)) in p.iter().zip(q).enumerate() {
            assert!((u - v).abs() <= tol, "{what}: sample {i} channel {c}: {p:?} vs {q:?}");
        }
    }
}

/// `whole` over `out` equals the four quadrants computed separately (window independence).
fn assert_window_independent(out: Rect, f: &dyn Fn(Rect) -> Vec<[f32; 4]>) {
    let whole = f(out);
    let (hw, hh) = (out.width / 2, out.height / 2);
    for (qx, qy, qw, qh) in [
        (0, 0, hw, hh),
        (hw, 0, out.width - hw, hh),
        (0, hh, hw, out.height - hh),
        (hw, hh, out.width - hw, out.height - hh),
    ] {
        let q = f(rect(out.x + i64::from(qx), out.y + i64::from(qy), qw, qh));
        for j in 0..qh {
            for i in 0..qw {
                let w = ((qy + j) * out.width + qx + i) as usize;
                assert_eq!(q[(j * qw + i) as usize], whole[w], "quadrant ({qx},{qy}) pixel ({i},{j})");
            }
        }
    }
}

mod box_kernel_tests {
    use super::*;

    /// Direct convolution of `iterations` boxes (sum in index order), the independent reference.
    fn reference(h: usize, iterations: u32) -> Vec<f64> {
        let w = 2 * h + 1;
        let mut cur = vec![1.0_f64];
        for _ in 0..iterations {
            let mut next = vec![0.0; cur.len() + w - 1];
            for (i, v) in cur.iter().enumerate() {
                for k in 0..w {
                    next[i + k] += v / w as f64;
                }
            }
            cur = next;
        }
        let s: f64 = cur.iter().sum();
        cur.iter().map(|v| v / s).collect()
    }

    #[test]
    fn box_kernel_goldens_identity_and_normalisation() {
        // Goldens: one box of width 3 is [1, 1, 1] / 3; two are [1, 2, 3, 2, 1] / 9.
        close1(&box_kernel(1, 1).unwrap(), &[1.0 / 3.0; 3], 1e-7);
        close1(&box_kernel(1, 2).unwrap(), &[1.0 / 9.0, 2.0 / 9.0, 3.0 / 9.0, 2.0 / 9.0, 1.0 / 9.0], 1e-7);
        close1(&box_kernel(2, 1).unwrap(), &[0.2; 5], 1e-7);
        assert_eq!(box_kernel(0, 7).unwrap(), vec![1.0]);
        assert_eq!(box_kernel(7, 0).unwrap(), vec![1.0]);
        for (h, it) in [(1u32, 1u32), (1, 3), (2, 3), (5, 3), (17, 2), (40, 5), (3000, 1)] {
            let t = box_kernel(h, it).unwrap();
            assert_eq!(t.len(), (2 * h * it + 1) as usize, "len ({h},{it})");
            let sum: f64 = t.iter().map(|v| f64::from(*v)).sum();
            assert!((sum - 1.0).abs() < 1e-5, "sum ({h},{it}) = {sum}");
            assert!(t.iter().all(|v| *v >= 0.0 && v.is_finite()));
            for (i, (a, b)) in t.iter().zip(t.iter().rev()).enumerate() {
                assert!((a - b).abs() <= 1e-7, "symmetry ({h},{it}) at {i}");
            }
            if h <= 40 {
                let r = reference(h as usize, it);
                for (a, b) in t.iter().zip(&r) {
                    assert!((f64::from(*a) - b).abs() <= 1e-7, "({h},{it}) {a} vs reference {b}");
                }
            }
        }
    }

    fn close1(a: &[f32], b: &[f64], tol: f64) {
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b) {
            assert!((f64::from(*x) - y).abs() <= tol, "{x} vs {y}");
        }
    }

    #[test]
    fn three_box_iterations_approximate_a_gaussian_within_the_stated_bound() {
        // Stated bound: three boxes of half-width h have the variance h (h + 1) exactly (three
        // times the box variance ((2h + 1)^2 - 1) / 12), and against the Gaussian of that variance
        // sampled on the same support and normalised, the largest tap difference is at most 10 % of
        // the Gaussian peak and the L1 difference at most 0.06 (measured: 9.2 % / 0.052 at h = 1,
        // 6.3 % / 0.051 at h = 25).
        for h in [1u32, 2, 3, 5, 10, 25] {
            let t = box_kernel(h, 3).unwrap();
            let c = (t.len() / 2) as f64;
            let var_want = f64::from(h) * f64::from(h + 1);
            let var: f64 = t.iter().enumerate().map(|(i, v)| f64::from(*v) * (i as f64 - c).powi(2)).sum();
            assert!((var - var_want).abs() <= 1e-4 * var_want, "h {h}: variance {var} vs {var_want}");
            let g: Vec<f64> = (0..t.len()).map(|i| (-(i as f64 - c).powi(2) / (2.0 * var_want)).exp()).collect();
            let gs: f64 = g.iter().sum();
            let g: Vec<f64> = g.iter().map(|v| v / gs).collect();
            let peak = g.iter().cloned().fold(0.0, f64::max);
            let max = t.iter().zip(&g).map(|(a, b)| (f64::from(*a) - b).abs()).fold(0.0, f64::max);
            let l1: f64 = t.iter().zip(&g).map(|(a, b)| (f64::from(*a) - b).abs()).sum();
            assert!(max <= 0.10 * peak, "h {h}: max difference {max} vs peak {peak}");
            assert!(l1 <= 0.06, "h {h}: L1 {l1}");
        }
    }

    #[test]
    fn box_kernel_refuses_more_than_the_tap_bound() {
        assert_eq!(box_kernel(3000, 1).unwrap().len(), MAX_BOX_KERNEL_TAPS);
        assert_eq!(box_kernel(3000, 2), Err(BlurError::Parameter));
        assert_eq!(box_kernel(1, 3001), Err(BlurError::Parameter));
        assert_eq!(box_kernel(u32::MAX, u32::MAX), Err(BlurError::Parameter));
        assert_eq!(BlurError::Parameter.code(), "blur_parameter");
        assert_eq!(FilterError::from(BlurError::Parameter), FilterError::Parameter);
    }
}

mod axis_tests {
    use super::*;

    #[test]
    fn one_axis_pass_golden_on_a_dot() {
        let (src, sr) = dot((2, 1), [1.0, 0.5, 0.0, 1.0]);
        let taps = box_kernel(1, 1).unwrap();
        let third = (f64::from(taps[0]) * 1.0) as f32;
        let h = blur_window_axis(UNIT, &src, sr, rect(0, 1, 5, 1), &taps, BlurAxis::Horizontal, WIDE).unwrap();
        let ink = [1.0, 0.5, 0.0, third];
        assert_eq!(h, vec![[0.0; 4], ink, ink, ink, [0.0; 4]]);
        let v = blur_window_axis(UNIT, &src, sr, rect(2, -1, 1, 5), &taps, BlurAxis::Vertical, WIDE).unwrap();
        assert_eq!(v, vec![[0.0; 4], ink, ink, ink, [0.0; 4]]);
        // The horizontal pass leaves other rows untouched (transparent here).
        let off = blur_window_axis(UNIT, &src, sr, rect(0, 0, 5, 1), &taps, BlurAxis::Horizontal, WIDE).unwrap();
        assert_eq!(off, vec![[0.0; 4]; 5]);
    }

    #[test]
    fn horizontal_then_vertical_equals_the_two_dimensional_blur() {
        let sr = rect(-1, 2, 6, 5);
        let src = plane(6, 5, 11);
        for radius in [1.0_f32, 2.0, 3.5] {
            let taps = LayerBlur::new(BlurScope::Layer, radius).taps().unwrap();
            let support = (taps.len() / 2) as i64;
            let out = rect(-4, -1, 12, 11);
            let mid = rect(out.x, out.y - support, out.width, out.height + 2 * support as u32);
            let h = blur_window_axis(UNIT, &src, sr, mid, &taps, BlurAxis::Horizontal, WIDE).unwrap();
            let hv = blur_window_axis(UNIT, &h, mid, out, &taps, BlurAxis::Vertical, WIDE).unwrap();
            let two_d = blur_window_in(UNIT, &src, sr, out, &taps, WIDE).unwrap();
            close(&hv, &two_d, 1e-6, &format!("radius {radius}"));
        }
        // Box taps too (the Fast Box Blur use).
        let taps = box_kernel(2, 3).unwrap();
        let s = (taps.len() / 2) as i64;
        let out = rect(-2, 0, 9, 9);
        let mid = rect(out.x, out.y - s, out.width, out.height + 2 * s as u32);
        let h = blur_window_axis(UNIT, &src, sr, mid, &taps, BlurAxis::Horizontal, WIDE).unwrap();
        let hv = blur_window_axis(UNIT, &h, mid, out, &taps, BlurAxis::Vertical, WIDE).unwrap();
        close(&hv, &blur_window_in(UNIT, &src, sr, out, &taps, WIDE).unwrap(), 1e-6, "box (2,3)");
    }

    #[test]
    fn axis_identity_window_independence_and_refusals() {
        let sr = rect(0, 0, 5, 4);
        let src = plane(5, 4, 3);
        let id = blur_window_axis(UNIT, &src, sr, sr, &[1.0], BlurAxis::Vertical, WIDE).unwrap();
        assert_eq!(id, src);
        let taps = box_kernel(2, 2).unwrap();
        for axis in [BlurAxis::Horizontal, BlurAxis::Vertical] {
            assert_window_independent(rect(-2, -1, 9, 7), &|w| {
                blur_window_axis(UNIT, &src, sr, w, &taps, axis, WIDE).unwrap()
            });
        }
        let out = rect(0, 0, 5, 4);
        assert_eq!(blur_window_axis(UNIT, &src, sr, out, &[0.5, 0.5], BlurAxis::Horizontal, WIDE), Err(BlurError::Shape));
        assert_eq!(blur_window_axis(UNIT, &src[1..], sr, out, &taps, BlurAxis::Horizontal, WIDE), Err(BlurError::Shape));
        let (bytes, work) = blur_window_axis_cost(out, taps.len()).unwrap();
        assert_eq!((bytes, work), (20 * 16, 20 * 9));
        let tight = BlurLimits { max_bytes: bytes, max_work: work - 1 };
        assert_eq!(
            blur_window_axis(UNIT, &src, sr, out, &taps, BlurAxis::Horizontal, tight),
            Err(BlurError::OverBudget { needed: work, limit: work - 1 })
        );
        let huge = rect(0, 0, 1 << 20, 1 << 20);
        assert!(matches!(
            blur_window_axis(UNIT, &src, sr, huge, &taps, BlurAxis::Horizontal, WIDE),
            Err(BlurError::OverBudget { .. })
        ));
        let mut bad = src.clone();
        bad[6] = [1.5, 0.0, 0.0, 1.0];
        assert_eq!(blur_window_axis(UNIT, &bad, sr, out, &taps, BlurAxis::Horizontal, WIDE), Err(BlurError::OutOfDomain));
        // The extended range admits the same sample (scene-referred colour above 1).
        assert!(blur_window_axis(SampleRange::Extended, &bad, sr, out, &taps, BlurAxis::Horizontal, WIDE).is_ok());
    }
}

mod directional_tests {
    use super::*;

    #[test]
    fn directional_goldens_on_a_dot() {
        let (src, sr) = dot((5, 3), [0.2, 0.4, 0.6, 1.0]);
        // Length 2 along 0 degrees: three samples at offsets -1, 0, 1, each 1/3.
        let got = directional_blur_window(UNIT, &src, sr, rect(3, 3, 5, 1), 0.0, 2.0, WIDE).unwrap();
        let a = (1.0_f64 / 3.0) as f32;
        let ink = |alpha: f32| [0.2, 0.4, 0.6, alpha];
        close(&got, &[[0.0; 4], ink(a), ink(a), ink(a), [0.0; 4]], 2e-7, "length 2");
        // Length 1: m = 1, step 0.5: the dot's own pixel gets (0.5 + 1 + 0.5) / 3, its neighbours
        // 0.5 / 3 (the declared bilinear filter).
        let got = directional_blur_window(UNIT, &src, sr, rect(3, 3, 5, 1), 0.0, 1.0, WIDE).unwrap();
        close(&got, &[[0.0; 4], ink(1.0 / 6.0), ink(2.0 / 3.0), ink(1.0 / 6.0), [0.0; 4]], 2e-7, "length 1");
        // 90 degrees is vertical.
        let got = directional_blur_window(UNIT, &src, sr, rect(5, 1, 1, 5), 90.0, 2.0, WIDE).unwrap();
        close(&got, &[[0.0; 4], ink(a), ink(a), ink(a), [0.0; 4]], 2e-7, "vertical");
    }

    #[test]
    fn angle_zero_is_the_horizontal_box_and_length_zero_is_identity() {
        let sr = rect(0, 0, 7, 6);
        let src = plane(7, 6, 29);
        let out = rect(-3, -2, 13, 10);
        for h in [1u32, 2, 4] {
            let boxed = blur_window_axis(UNIT, &src, sr, out, &box_kernel(h, 1).unwrap(), BlurAxis::Horizontal, WIDE).unwrap();
            let vboxed = blur_window_axis(UNIT, &src, sr, out, &box_kernel(h, 1).unwrap(), BlurAxis::Vertical, WIDE).unwrap();
            let len = (2 * h) as f32;
            for angle in [0.0_f32, 180.0, 360.0, -180.0, 720.0] {
                let d = directional_blur_window(UNIT, &src, sr, out, angle, len, WIDE).unwrap();
                close(&d, &boxed, 1e-6, &format!("h {h} angle {angle}"));
            }
            for angle in [90.0_f32, 270.0, -90.0] {
                let d = directional_blur_window(UNIT, &src, sr, out, angle, len, WIDE).unwrap();
                close(&d, &vboxed, 1e-6, &format!("h {h} angle {angle}"));
            }
        }
        for angle in [0.0_f32, 37.0, -123.5] {
            assert_eq!(directional_blur_window(UNIT, &src, sr, sr, angle, 0.0, WIDE).unwrap(), src, "angle {angle}");
        }
    }

    #[test]
    fn oblique_directional_blur_conserves_mass_along_the_line() {
        let (src, sr) = dot((20, 20), [1.0, 1.0, 1.0, 1.0]);
        let out = rect(0, 0, 41, 41);
        for (angle, len) in [(45.0_f32, 10.0_f32), (30.0, 7.3), (-60.0, 15.0)] {
            let got = directional_blur_window(UNIT, &src, sr, out, angle, len, WIDE).unwrap();
            let (c, s) = (f64::from(angle).to_radians().cos(), f64::from(angle).to_radians().sin());
            let (mut mass, mut mx, mut my) = (0.0_f64, 0.0_f64, 0.0_f64);
            for (i, p) in got.iter().enumerate() {
                let (x, y) = ((i % 41) as f64, (i / 41) as f64);
                let a = f64::from(p[3]);
                mass += a;
                mx += a * x;
                my += a * y;
                if a > 1e-6 {
                    // Within the bilinear support of the segment through the dot (direction (c, -s)).
                    let (dx, dy) = (x - 20.0, y - 20.0);
                    let perp = (dx * s + dy * c).abs();
                    let along = (dx * c - dy * s).abs();
                    assert!(perp <= 1.5 && along <= f64::from(len) / 2.0 + 1.5, "angle {angle}: ({x},{y}) a {a}");
                }
            }
            assert!((mass - 1.0).abs() < 1e-5, "angle {angle}: mass {mass}");
            assert!((mx / mass - 20.0).abs() < 1e-5 && (my / mass - 20.0).abs() < 1e-5, "angle {angle}: centroid");
        }
    }

    #[test]
    fn directional_window_independence_and_refusals() {
        let sr = rect(0, 0, 6, 6);
        let src = plane(6, 6, 5);
        assert_window_independent(rect(-3, -3, 12, 11), &|w| {
            directional_blur_window(UNIT, &src, sr, w, 33.0, 5.5, WIDE).unwrap()
        });
        let out = rect(0, 0, 6, 6);
        for (angle, len) in [(f32::NAN, 2.0), (f32::INFINITY, 2.0), (0.0, -1.0), (0.0, 6000.5), (0.0, f32::NAN)] {
            assert_eq!(
                directional_blur_window(UNIT, &src, sr, out, angle, len, WIDE),
                Err(BlurError::Parameter),
                "angle {angle} length {len}"
            );
        }
        assert!(directional_blur_window(UNIT, &src, sr, rect(0, 0, 1, 1), 0.0, 6000.0, WIDE).is_ok());
        let (bytes, work) = directional_blur_window_cost(out, 5.5).unwrap();
        assert_eq!((bytes, work), (36 * 16, 36 * 7 * 4));
        assert_eq!(
            directional_blur_window(UNIT, &src, sr, out, 10.0, 5.5, BlurLimits { max_bytes: bytes - 1, max_work: work }),
            Err(BlurError::OverBudget { needed: bytes, limit: bytes - 1 })
        );
        assert!(directional_blur_window(UNIT, &src, sr, out, 10.0, 5.5, BlurLimits { max_bytes: bytes, max_work: work }).is_ok());
        let mut bad = src.clone();
        bad[0] = [0.0, f32::NAN, 0.0, 1.0];
        assert_eq!(directional_blur_window(UNIT, &bad, sr, out, 0.0, 3.0, WIDE), Err(BlurError::OutOfDomain));
    }
}

mod radial_tests {
    use super::*;

    // Plan centre (10.5, 10.5) is the centre of pixel (10, 10).
    const C: [f32; 2] = [10.5, 10.5];

    #[test]
    fn radial_goldens() {
        // Spin 200 (72 degrees) at distance 1: m = 1, samples at -36, 0, +36 degrees; a sample
        // rotated by phi lands at centre + (cos phi, +-sin phi), putting bilinear weight
        // cos phi * (1 - sin phi) on the dot pixel: alpha (1 + 2 cos 36 (1 - sin 36)) / 3.
        let (src, sr) = dot((11, 10), [1.0, 1.0, 1.0, 1.0]);
        let got = radial_blur_window(UNIT, &src, sr, rect(11, 10, 1, 1), C, 200.0, RadialBlurKind::Spin, WIDE).unwrap();
        let phi = 36.0_f64.to_radians();
        let want = (1.0 + 2.0 * phi.cos() * (1.0 - phi.sin())) / 3.0;
        assert!((f64::from(got[0][3]) - want).abs() < 1e-6, "{got:?} vs {want}");
        assert!((got[0][0] - 1.0).abs() < 1e-6);
        // Zoom 1000 at distance 2: m = 1, scales 0.5, 1, 1.5: the pixels at distance 1, 2, 3 on the ray.
        let src = vec![[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]];
        let got = radial_blur_window(UNIT, &src, rect(11, 10, 3, 1), rect(12, 10, 1, 1), C, 1000.0, RadialBlurKind::Zoom, WIDE)
            .unwrap();
        close(&got, &[[1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0, 1.0]], 1e-6, "zoom");
        // Amount 0 is the identity for both kinds; the centre pixel itself is never moved.
        let sr = rect(5, 5, 11, 11);
        let p = plane(11, 11, 17);
        for kind in [RadialBlurKind::Spin, RadialBlurKind::Zoom] {
            assert_eq!(radial_blur_window(UNIT, &p, sr, sr, C, 0.0, kind, WIDE).unwrap(), p);
            let at_centre = radial_blur_window(UNIT, &p, sr, rect(10, 10, 1, 1), C, 1000.0, kind, WIDE).unwrap();
            close(&at_centre, &[p[5 * 11 + 5]], 1e-6, "centre pixel");
        }
    }

    /// Alpha-weighted statistics of the output of a dot at `(22, 10)` (distance 12, angle 0).
    fn dot_stats(kind: RadialBlurKind, amount: f32) -> (Vec<(f64, f64, f64)>, f64, f64) {
        let (src, sr) = dot((22, 10), [1.0, 1.0, 1.0, 1.0]);
        let out = rect(-5, -5, 45, 35);
        let got = radial_blur_window(UNIT, &src, sr, out, C, amount, kind, WIDE).unwrap();
        let mut pts = Vec::new();
        let (mut mass, mut md, mut ma) = (0.0, 0.0, 0.0);
        for (i, p) in got.iter().enumerate() {
            let a = f64::from(p[3]);
            if a <= 1e-6 {
                continue;
            }
            let (x, y) = ((i % 45) as f64 - 5.0, (i / 45) as f64 - 5.0);
            let (dx, dy) = (x - 10.0, y - 10.0);
            let (d, ang) = ((dx * dx + dy * dy).sqrt(), dy.atan2(dx).to_degrees());
            pts.push((d, ang, a));
            mass += a;
            md += a * d;
            ma += a * ang;
        }
        (pts, md / mass, ma / mass)
    }

    #[test]
    fn spin_preserves_radial_distance() {
        let (pts, mean_d, mean_a) = dot_stats(RadialBlurKind::Spin, 100.0);
        for (d, ang, a) in &pts {
            assert!((d - 12.0).abs() <= 1.5, "pixel at distance {d}, angle {ang}, alpha {a}");
        }
        assert!((mean_d - 12.0).abs() <= 0.25, "mean distance {mean_d}");
        assert!(mean_a.abs() <= 0.5, "mean angle {mean_a}");
        let spread = pts.iter().map(|p| p.1.abs()).fold(0.0, f64::max);
        assert!(spread >= 15.0, "spin spread only {spread} degrees");
    }

    #[test]
    fn zoom_preserves_angle() {
        let (pts, mean_d, mean_a) = dot_stats(RadialBlurKind::Zoom, 500.0);
        let bound = (1.5_f64 / 12.0).asin().to_degrees();
        for (d, ang, a) in &pts {
            assert!(ang.abs() <= bound, "pixel at distance {d}, angle {ang} (bound {bound}), alpha {a}");
        }
        assert!(mean_a.abs() <= 0.5, "mean angle {mean_a}");
        let (lo, hi) = pts.iter().fold((f64::MAX, 0.0_f64), |(l, h), p| (l.min(p.0), h.max(p.0)));
        assert!(lo <= 10.0 && hi >= 15.0, "zoom spread {lo}..{hi} (mean {mean_d})");
    }

    #[test]
    fn radial_window_independence_cost_and_refusals() {
        let sr = rect(0, 0, 9, 8);
        let src = plane(9, 8, 41);
        for kind in [RadialBlurKind::Spin, RadialBlurKind::Zoom] {
            assert_window_independent(rect(-2, -1, 13, 11), &|w| {
                radial_blur_window(UNIT, &src, sr, w, [4.3, 3.9], 350.0, kind, WIDE).unwrap()
            });
        }
        let out = rect(0, 0, 9, 8);
        for (centre, amount) in [([f32::NAN, 0.0], 10.0), ([0.0, f32::INFINITY], 10.0), ([0.0, 0.0], -1.0), ([0.0, 0.0], 1000.5), ([0.0, 0.0], f32::NAN)] {
            for kind in [RadialBlurKind::Spin, RadialBlurKind::Zoom] {
                assert_eq!(radial_blur_window(UNIT, &src, sr, out, centre, amount, kind, WIDE), Err(BlurError::Parameter));
                assert_eq!(radial_blur_window_cost(out, centre, amount, kind), Err(BlurError::Parameter));
            }
        }
        // The predicted work is exact: it admits the call, one less refuses it before allocation.
        let (bytes, work) = radial_blur_window_cost(out, [0.0, 0.0], 80.0, RadialBlurKind::Zoom).unwrap();
        assert_eq!(bytes, 72 * 16);
        assert!(work > 72 * 4 && work % 4 == 0);
        let ok = BlurLimits { max_bytes: bytes, max_work: work };
        assert!(radial_blur_window(UNIT, &src, sr, out, [0.0, 0.0], 80.0, RadialBlurKind::Zoom, ok).is_ok());
        let tight = BlurLimits { max_bytes: bytes, max_work: work - 1 };
        assert_eq!(
            radial_blur_window(UNIT, &src, sr, out, [0.0, 0.0], 80.0, RadialBlurKind::Zoom, tight),
            Err(BlurError::OverBudget { needed: work, limit: work - 1 })
        );
        // Far from the centre the per-pixel sample count is capped (2 * 256 + 1).
        let far = radial_blur_window_cost(rect(100_000, 0, 1, 1), [0.0, 0.0], 100.0, RadialBlurKind::Spin).unwrap();
        assert_eq!(far, (16, 513 * 4));
        let mut bad = src.clone();
        bad[40] = [0.0, 0.0, 0.0, 1.5];
        assert_eq!(
            radial_blur_window(UNIT, &bad, sr, out, [4.5, 4.5], 50.0, RadialBlurKind::Spin, WIDE),
            Err(BlurError::OutOfDomain)
        );
    }

    /// F-R43-3: a hostile output rectangle is refused against the caller's limits BEFORE the
    /// per-pixel sample walk (area x 16 bytes, area x 4 work: at least one sample of four corners
    /// per pixel), so the call returns without visiting its pixels or allocating. Without the
    /// pre-check the walk visits 10^12 (or ~1.8 x 10^19) pixels and the test runs into the
    /// per-test cap.
    #[test]
    fn radial_hostile_rect_is_refused_before_the_walk() {
        let (src, sr) = dot((0, 0), [1.0; 4]);
        let side = 1_000_000_u32;
        let huge = rect(0, 0, side, side);
        let area = u64::from(side) * u64::from(side);
        let small = BlurLimits { max_bytes: 1 << 20, max_work: 1 << 24 };
        let bytes_only = BlurLimits { max_bytes: u64::MAX, max_work: 1 << 24 };
        let widest = rect(-5, 7, u32::MAX, u32::MAX);
        for kind in [RadialBlurKind::Spin, RadialBlurKind::Zoom] {
            for amount in [0.0, 100.0, 1000.0] {
                assert_eq!(
                    radial_blur_window(UNIT, &src, sr, huge, [0.0, 0.0], amount, kind, small),
                    Err(BlurError::OverBudget { needed: area * 16, limit: 1 << 20 })
                );
                assert_eq!(
                    radial_blur_window(UNIT, &src, sr, huge, [0.0, 0.0], amount, kind, bytes_only),
                    Err(BlurError::OverBudget { needed: area * 4, limit: 1 << 24 })
                );
                // u32::MAX x u32::MAX: the byte product overflows u64 (checked, typed).
                assert_eq!(radial_blur_window(UNIT, &src, sr, widest, [0.0, 0.0], amount, kind, small), Err(BlurError::Overflow));
            }
        }
    }
}

mod find_edges_tests {
    use super::*;

    fn step(w: u32, h: u32, left: [f32; 4], right: [f32; 4]) -> Vec<[f32; 4]> {
        (0..w * h).map(|i| if i % w < w / 2 { left } else { right }).collect()
    }

    #[test]
    fn find_edges_golden_on_a_step_edge() {
        let (w, h) = (8u32, 4u32);
        let sr = rect(0, 0, w, h);
        let src = step(w, h, [0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 1.0]);
        // Columns 3 and 4 straddle the step: gx = 4, m = 1. Elsewhere (and across the replicated
        // border) the gradient is 0.
        let plain = find_edges_window(&src, sr, sr, false, FWIDE).unwrap();
        let inv = find_edges_window(&src, sr, sr, true, FWIDE).unwrap();
        for (i, (p, q)) in plain.iter().zip(&inv).enumerate() {
            let x = i as u32 % w;
            let edge = x == 3 || x == 4;
            let want = if edge { 0.0 } else { 1.0 };
            assert_eq!(*p, [want, want, want, 1.0], "plain x {x}");
            assert_eq!(*q, [1.0 - want, 1.0 - want, 1.0 - want, 1.0], "invert x {x}");
        }
        // A half step (0 to 0.5) gives m = 0.5; a red-only step leaves green and blue flat.
        let half = step(w, h, [0.0, 0.0, 0.0, 1.0], [0.5, 0.5, 0.5, 1.0]);
        assert_eq!(find_edges_window(&half, sr, rect(3, 1, 1, 1), true, FWIDE).unwrap(), vec![[0.5, 0.5, 0.5, 1.0]]);
        let red = step(w, h, [0.0, 0.2, 0.2, 1.0], [1.0, 0.2, 0.2, 1.0]);
        assert_eq!(find_edges_window(&red, sr, rect(4, 2, 1, 1), false, FWIDE).unwrap(), vec![[0.0, 1.0, 1.0, 1.0]]);
        // A horizontal step (rows 0-1 black, 2-3 white) is found by gy on rows 1 and 2.
        let rows: Vec<[f32; 4]> = (0..w * h).map(|i| if i / w < 2 { [0.0, 0.0, 0.0, 1.0] } else { [1.0; 4] }).collect();
        let got = find_edges_window(&rows, sr, sr, true, FWIDE).unwrap();
        for (i, p) in got.iter().enumerate() {
            let y = i as u32 / w;
            let want = if y == 1 || y == 2 { 1.0 } else { 0.0 };
            assert_eq!(*p, [want, want, want, 1.0], "row {y}");
        }
    }

    #[test]
    fn find_edges_flat_transparent_and_beyond_the_source() {
        let sr = rect(2, 2, 5, 4);
        let flat = vec![[0.3, 0.6, 0.9, 1.0]; 20];
        let got = find_edges_window(&flat, sr, rect(0, 0, 9, 8), false, FWIDE).unwrap();
        for (i, p) in got.iter().enumerate() {
            let (x, y) = (i as i64 % 9, i as i64 / 9);
            let inside = (2..7).contains(&x) && (2..6).contains(&y);
            assert_eq!(*p, if inside { [1.0, 1.0, 1.0, 1.0] } else { [0.0; 4] }, "({x},{y})");
        }
        // Alpha is the source pixel's; a transparent pixel stays transparent; premultiplied, so a
        // transparent neighbour is an edge of the layer.
        let mut holes = flat.clone();
        holes[7] = [0.3, 0.6, 0.9, 0.0];
        holes[0] = [0.3, 0.6, 0.9, 0.5];
        let got = find_edges_window(&holes, sr, sr, true, FWIDE).unwrap();
        assert_eq!(got[7], [0.0; 4]);
        assert_eq!(got[0][3], 0.5);
        assert!(got[6][2] > 0.0 && got[8][2] > 0.0);
        assert_window_independent(rect(1, 1, 7, 6), &|win| find_edges_window(&holes, sr, win, false, FWIDE).unwrap());
    }

    #[test]
    fn find_edges_refusals() {
        let sr = rect(0, 0, 4, 4);
        let src = vec![[0.5; 4]; 16];
        assert_eq!(find_edges_window(&src[1..], sr, sr, false, FWIDE), Err(FilterError::Shape));
        let (bytes, work) = find_edges_window_cost(sr).unwrap();
        assert_eq!((bytes, work), (16 * 16, 16 * 9));
        assert_eq!(
            find_edges_window(&src, sr, sr, false, FilterLimits { max_bytes: bytes, max_work: work - 1 }),
            Err(FilterError::OverBudget { needed: work, limit: work - 1 })
        );
        assert!(matches!(
            find_edges_window(&src, sr, rect(0, 0, 1 << 20, 1 << 20), false, FWIDE),
            Err(FilterError::OverBudget { .. })
        ));
        let mut bad = src.clone();
        bad[5] = [1.5, 0.0, 0.0, 1.0];
        assert_eq!(find_edges_window(&bad, sr, sr, false, FWIDE), Err(FilterError::OutOfDomain));
        bad[5] = [0.0, 0.0, 0.0, f32::NAN];
        assert_eq!(find_edges_window(&bad, sr, sr, false, FWIDE), Err(FilterError::OutOfDomain));
    }
}
