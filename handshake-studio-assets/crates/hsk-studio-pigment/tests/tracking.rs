//! Point tracker and solves (lane SMB-AE-KEY-TRACK slice 2). Synthetic sequences with KNOWN motion:
//! every frame renders a smooth analytic texture through the exact inverse motion, so the true
//! feature position of every frame is known in closed form. Accuracy bounds are stated per test.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::track_math::*;

const W: u32 = 128;

/// Smooth texture with structure in every direction, values inside `[0.02, 0.98]`.
fn tex(x: f64, y: f64) -> f64 {
    0.5 + 0.18 * (0.31 * x + 0.17 * y).sin()
        + 0.14 * (0.13 * x - 0.29 * y + 1.0).sin()
        + 0.1 * (0.53 * x + 0.41 * y + 2.0).sin()
        + 0.06 * (0.07 * x + 0.11 * y + 0.5).sin()
}

/// Pixel `(i, j)` samples the texture at the source point of its centre.
fn render(w: u32, h: u32, inv: impl Fn(f64, f64) -> (f64, f64), f: impl Fn(f64, f64) -> f64) -> LumaFrame {
    let mut data = Vec::new();
    for j in 0..h {
        for i in 0..w {
            let (x, y) = inv(f64::from(i) + 0.5, f64::from(j) + 0.5);
            data.push(f(x, y) as f32);
        }
    }
    LumaFrame::new(w, h, data).unwrap()
}

fn spec(x: f64, y: f64, r: u32, s: u32) -> TrackPointSpec {
    TrackPointSpec { position: [x, y], feature_radius: r, search_radius: s }
}

fn run(frames: &[LumaFrame], start: usize, dir: TrackDirection, pts: &[TrackPointSpec]) -> Vec<Vec<TrackSample>> {
    track_points(frames, start, dir, pts, &TrackParams::DEFAULT, &TrackLimits::DEFAULT, &CancellationToken::default(), &mut |_, _| {}).unwrap()
}

fn err(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

#[test]
fn translation_is_tracked_subpixel_forward_and_backward() {
    let d = [1.7, -0.9];
    let frames: Vec<LumaFrame> = (0..12)
        .map(|t| render(W, W, |x, y| (x - d[0] * f64::from(t), y - d[1] * f64::from(t)), tex))
        .collect();
    let pts = [spec(40.0, 60.0, 7, 8), spec(70.0, 70.0, 7, 8), spec(90.0, 50.0, 7, 8)];
    let tracks = run(&frames, 0, TrackDirection::Forward, &pts);
    let mut worst = 0.0f64;
    for (p, tr) in pts.iter().zip(&tracks) {
        assert_eq!(tr.len(), 12);
        for s in tr {
            let t = s.frame as f64;
            let truth = [p.position[0] + d[0] * t, p.position[1] + d[1] * t];
            worst = worst.max(err(s.position, truth));
            assert_eq!(s.status, TrackStatus::Tracked, "{s:?}");
            assert!(s.confidence > 0.99, "{s:?}");
        }
    }
    println!("translation: worst error {worst:.5} px over 11 frame pairs");
    assert!(worst < 0.05, "subpixel accuracy bound 0.05 px, measured {worst}");
    // One-point solve = offset from the reference frame.
    let off = solve_one_point(&tracks[0], 0).unwrap();
    assert!((off[11].1[0] - 11.0 * d[0]).abs() < 0.05 && (off[11].1[1] - 11.0 * d[1]).abs() < 0.05);
    // Backward from the last frame with the true end position recovers the start.
    let end = [40.0 + 11.0 * d[0], 60.0 + 11.0 * d[1]];
    let back = run(&frames, 11, TrackDirection::Backward, &[spec(end[0], end[1], 7, 8)]);
    assert_eq!(back[0].first().unwrap().frame, 0, "samples are in frame order");
    assert!(err(back[0][0].position, [40.0, 60.0]) < 0.05, "{:?}", back[0][0]);
}

fn mat_inv(m: &[f64; 9]) -> [f64; 9] {
    let [a, b, c, d, e, f, g, h, i] = *m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    [
        (e * i - f * h) / det,
        (c * h - b * i) / det,
        (b * f - c * e) / det,
        (f * g - d * i) / det,
        (a * i - c * g) / det,
        (c * d - a * f) / det,
        (d * h - e * g) / det,
        (b * g - a * h) / det,
        (a * e - b * d) / det,
    ]
}

fn hap(m: &[f64; 9], p: [f64; 2]) -> [f64; 2] {
    let w = m[6] * p[0] + m[7] * p[1] + m[8];
    [(m[0] * p[0] + m[1] * p[1] + m[2]) / w, (m[3] * p[0] + m[4] * p[1] + m[5]) / w]
}

#[test]
fn two_point_rotation_scale_and_four_point_corner_pin() {
    // Rotation 2 degrees and scale 1.01 per frame about (64, 64).
    let c = [64.0, 64.0];
    let sim = |t: f64| {
        let (s, a) = (1.01f64.powf(t), (2.0 * t).to_radians());
        [s * a.cos(), -s * a.sin(), s * a.sin(), s * a.cos()]
    };
    let frames: Vec<LumaFrame> = (0..8)
        .map(|t| {
            let m = sim(f64::from(t));
            let det = m[0] * m[3] - m[1] * m[2];
            render(W, W, |x, y| {
                let (dx, dy) = (x - c[0], y - c[1]);
                (c[0] + (m[3] * dx - m[1] * dy) / det, c[1] + (-m[2] * dx + m[0] * dy) / det)
            }, tex)
        })
        .collect();
    let a = spec(44.0, 64.0, 8, 8);
    let b = spec(84.0, 64.0, 8, 8);
    let tracks = run(&frames, 0, TrackDirection::Forward, &[a, b]);
    let solved = solve_two_point(&tracks[0], &tracks[1], 0).unwrap();
    let (mut worst_rot, mut worst_scale, mut worst_pos) = (0.0f64, 0.0f64, 0.0f64);
    for s in &solved {
        let t = s.frame as f64;
        worst_rot = worst_rot.max((s.rotation_deg - 2.0 * t).abs());
        worst_scale = worst_scale.max((s.scale - 1.01f64.powf(t)).abs());
        let m = sim(t);
        let (dx, dy) = (a.position[0] - c[0], a.position[1] - c[1]);
        let truth = [c[0] + m[0] * dx + m[1] * dy - a.position[0], c[1] + m[2] * dx + m[3] * dy - a.position[1]];
        worst_pos = worst_pos.max(err(s.offset, truth));
    }
    println!("two-point: worst rotation {worst_rot:.5} deg, scale {worst_scale:.6}, offset {worst_pos:.5} px");
    assert!(worst_rot < 0.1 && worst_scale < 0.002 && worst_pos < 0.1);

    // Perspective: H_t = I + t/7 (H - I).
    let h = [1.05, 0.04, -3.0, 0.02, 0.98, 2.0, 0.0004, -0.0003, 1.0];
    let ht = |t: f64| -> [f64; 9] {
        let id = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let mut m = [0.0; 9];
        for k in 0..9 {
            m[k] = id[k] + t / 7.0 * (h[k] - id[k]);
        }
        m
    };
    let frames: Vec<LumaFrame> = (0..8)
        .map(|t| {
            let inv = mat_inv(&ht(f64::from(t)));
            render(W, W, |x, y| {
                let p = hap(&inv, [x, y]);
                (p[0], p[1])
            }, tex)
        })
        .collect();
    let corners = [spec(30.0, 30.0, 8, 8), spec(98.0, 30.0, 8, 8), spec(98.0, 98.0, 8, 8), spec(30.0, 98.0, 8, 8)];
    let tracks = run(&frames, 0, TrackDirection::Forward, &corners);
    let pins = solve_corner_pin([&tracks[0], &tracks[1], &tracks[2], &tracks[3]], 0).unwrap();
    let mut worst = 0.0f64;
    for (frame, hom) in &pins {
        let m = ht(*frame as f64);
        for p in [[64.0, 64.0], [30.0, 30.0], [98.0, 98.0], [50.0, 80.0]] {
            worst = worst.max(err(hom.apply(p), hap(&m, p)));
        }
    }
    println!("corner pin: worst mapped-point error {worst:.5} px");
    assert!(worst < 0.15, "corner pin bound 0.15 px, measured {worst}");
    assert!(Homography::fit(&[[0.0, 0.0], [1.0, 1.0], [2.0, 2.0], [3.0, 3.0]], &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]]).is_err());
}

#[test]
fn pyramid_reaches_large_motion_and_the_search_region_bounds_it() {
    // 10 px per frame exceeds half the shortest wavelength of `tex` (9.4 px), which no tracker can
    // disambiguate, so this sequence uses the same texture three times coarser (shortest 28 px).
    let d = [10.3, 6.4];
    let coarse = |x: f64, y: f64| tex(x / 3.0, y / 3.0);
    let frames: Vec<LumaFrame> = (0..6)
        .map(|t| render(W, W, |x, y| (x - d[0] * f64::from(t), y - d[1] * f64::from(t)), coarse))
        .collect();
    assert_eq!(pyramid_levels(6, 16, W, W), 2);
    let tracks = run(&frames, 0, TrackDirection::Forward, &[spec(30.0, 30.0, 6, 16)]);
    let mut worst = 0.0f64;
    for s in &tracks[0] {
        let t = s.frame as f64;
        worst = worst.max(err(s.position, [30.0 + d[0] * t, 30.0 + d[1] * t]));
        assert!(s.status.is_tracked(), "{s:?}");
    }
    println!("large motion (10, 6) px/frame: worst error {worst:.5} px");
    assert!(worst < 0.05);
    // The same motion with an 8 px search region: lost on the first step, and it stays lost.
    let tracks = run(&frames, 0, TrackDirection::Forward, &[spec(30.0, 30.0, 6, 8)]);
    assert_eq!(tracks[0][1].status, TrackStatus::Lost);
    assert_eq!(tracks[0][1].position, [30.0, 30.0]);
    assert!(tracks[0][2..].iter().all(|s| s.status == TrackStatus::Lost && s.confidence == 0.0));
    // A feature moving out of a narrow frame.
    let narrow: Vec<LumaFrame> = (0..12).map(|t| render(40, 40, |x, y| (x - 3.0 * f64::from(t), y), tex)).collect();
    let tr = run(&narrow, 0, TrackDirection::Forward, &[spec(30.0, 20.0, 4, 6)]);
    assert!(tr[0].iter().any(|s| s.status == TrackStatus::OutOfFrame), "{:?}", tr[0].last());
}

#[test]
fn degenerate_texture_and_occlusion_lower_status_and_confidence() {
    let flat: Vec<LumaFrame> = (0..3).map(|_| render(64, 64, |x, y| (x, y), |_, _| 0.5)).collect();
    let tr = run(&flat, 0, TrackDirection::Forward, &[spec(32.0, 32.0, 6, 6)]);
    assert!(tr[0].iter().all(|s| s.status == TrackStatus::Degenerate && s.confidence == 0.0 && s.position == [32.0, 32.0]));
    // Aperture problem: vertical stripes have no vertical gradient.
    let stripes: Vec<LumaFrame> = (0..3).map(|t| render(64, 64, |x, y| (x - f64::from(t), y), |x, _| 0.5 + 0.3 * (0.4 * x).sin())).collect();
    let tr = run(&stripes, 0, TrackDirection::Forward, &[spec(32.0, 32.0, 6, 6)]);
    assert_eq!(tr[0][0].status, TrackStatus::Degenerate);
    // Occlusion: frame 3 shows a different texture where the feature was.
    let other = |x: f64, y: f64| 0.5 + 0.3 * (0.21 * x * y / 40.0 + 0.9 * x).cos();
    let frames: Vec<LumaFrame> = (0..5)
        .map(|t| {
            let shift = f64::from(t);
            if t == 3 { render(W, W, |x, y| (x, y), other) } else { render(W, W, |x, y| (x - shift, y), tex) }
        })
        .collect();
    let tr = run(&frames, 0, TrackDirection::Forward, &[spec(60.0, 60.0, 7, 8)]);
    assert!(tr[0][1].status == TrackStatus::Tracked && tr[0][2].status == TrackStatus::Tracked);
    assert!(tr[0][3].confidence < TrackParams::DEFAULT.min_confidence, "{:?}", tr[0][3]);
    assert_ne!(tr[0][3].status, TrackStatus::Tracked);
}

#[test]
fn hostile_inputs_are_refused_before_allocation() {
    let f = render(32, 32, |x, y| (x, y), tex);
    let frames = vec![f.clone(), f.clone()];
    let call = |frames: &[LumaFrame], pts: &[TrackPointSpec], limits: TrackLimits| {
        track_points(frames, 0, TrackDirection::Forward, pts, &TrackParams::DEFAULT, &limits, &CancellationToken::default(), &mut |_, _| {}).unwrap_err()
    };
    let l = TrackLimits::DEFAULT;
    assert_eq!(call(&frames, &[spec(16.0, 16.0, 4, 1_000_000)], l), TrackError::Parameter, "huge search region");
    assert_eq!(call(&frames, &[spec(16.0, 16.0, 0, 4)], l), TrackError::Parameter);
    assert_eq!(call(&frames, &[spec(f64::NAN, 16.0, 4, 4)], l), TrackError::Parameter);
    assert_eq!(call(&frames, &[spec(16.0, 99.0, 4, 4)], l), TrackError::Parameter, "start outside the frame");
    assert_eq!(call(&frames, &[], l), TrackError::Parameter);
    let bad = TrackParams { epsilon_px: f64::NAN, ..TrackParams::DEFAULT };
    let e = track_points(&frames, 0, TrackDirection::Forward, &[spec(16.0, 16.0, 4, 4)], &bad, &l, &CancellationToken::default(), &mut |_, _| {});
    assert_eq!(e.unwrap_err(), TrackError::Parameter);
    // Frames: NaN, dims_u32_max with no data, mismatched sizes, over the pixel and work budgets.
    assert_eq!(LumaFrame::new(2, 1, vec![0.5, f32::NAN]).unwrap_err(), TrackError::OutOfDomain);
    assert_eq!(LumaFrame::new(u32::MAX, u32::MAX, vec![]).unwrap_err(), TrackError::Shape);
    let small = render(16, 16, |x, y| (x, y), tex);
    assert_eq!(call(&[f.clone(), small], &[spec(8.0, 8.0, 4, 4)], l), TrackError::Shape);
    let tight = TrackLimits { max_frame_pixels: 100, ..l };
    assert_eq!(call(&frames, &[spec(16.0, 16.0, 4, 4)], tight), TrackError::OverBudget { needed: 1024, limit: 100 });
    let tight = TrackLimits { max_work: 1_000, ..l };
    assert!(matches!(call(&frames, &[spec(16.0, 16.0, 4, 4)], tight), TrackError::OverBudget { limit: 1_000, .. }));
    // Cancellation and determinate progress.
    let many: Vec<LumaFrame> = (0..5).map(|_| f.clone()).collect();
    let mut seen = Vec::new();
    let ok = track_points(&many, 0, TrackDirection::Forward, &[spec(16.0, 16.0, 4, 4)], &TrackParams::DEFAULT, &l, &CancellationToken::default(), &mut |d, t| seen.push((d, t)));
    assert!(ok.is_ok());
    assert_eq!(seen, vec![(1, 5), (2, 5), (3, 5), (4, 5), (5, 5)]);
    let cancel = CancellationToken::default();
    cancel.cancel();
    let e = track_points(&many, 0, TrackDirection::Forward, &[spec(16.0, 16.0, 4, 4)], &TrackParams::DEFAULT, &l, &cancel, &mut |_, _| {});
    assert_eq!(e.unwrap_err(), TrackError::Canceled);
    // Solves on degenerate input.
    let still = ok.unwrap();
    assert_eq!(solve_two_point(&still[0], &still[0], 0).unwrap_err(), TrackError::Degenerate);
    assert_eq!(solve_two_point(&still[0], &still[0][..2], 0).unwrap_err(), TrackError::Shape);
}
