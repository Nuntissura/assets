//! Planar tracker (lane SMB-AE-KEY-TRACK slice 4): a textured plane under a known perspective ramp,
//! partly covered by an occluder moving on its own; RANSAC must keep the plane's homography.
use hsk_studio_accord::CancellationToken;
use hsk_studio_pigment::track_math::*;

fn tex(x: f64, y: f64) -> f64 {
    0.5 + 0.18 * (0.31 * x + 0.17 * y).sin()
        + 0.14 * (0.13 * x - 0.29 * y + 1.0).sin()
        + 0.1 * (0.53 * x + 0.41 * y + 2.0).sin()
        + 0.06 * (0.07 * x + 0.11 * y + 0.5).sin()
}

fn other(x: f64, y: f64) -> f64 {
    0.5 + 0.25 * (0.23 * x - 0.37 * y).cos() + 0.15 * (0.41 * x + 0.19 * y + 0.7).sin()
}

fn hap(m: &[f64; 9], p: [f64; 2]) -> [f64; 2] {
    let w = m[6] * p[0] + m[7] * p[1] + m[8];
    [(m[0] * p[0] + m[1] * p[1] + m[2]) / w, (m[3] * p[0] + m[4] * p[1] + m[5]) / w]
}

fn inv(m: &[f64; 9]) -> [f64; 9] {
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

/// The plane's homography at frame `t` (a ramp from identity).
fn ht(t: f64) -> [f64; 9] {
    let h = [1.06, 0.05, -4.0, 0.03, 0.97, 3.0, 0.0005, -0.0004, 1.0];
    let id = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let mut m = [0.0; 9];
    for k in 0..9 {
        m[k] = id[k] + t / 7.0 * (h[k] - id[k]);
    }
    m
}

fn frames() -> Vec<LumaFrame> {
    (0..8)
        .map(|t| {
            let tf = f64::from(t);
            let hi = inv(&ht(tf));
            let mut data = Vec::new();
            for j in 0..128u32 {
                for i in 0..128u32 {
                    let (x, y) = (f64::from(i) + 0.5, f64::from(j) + 0.5);
                    // An occluder window fixed on screen, showing its own texture drifting.
                    let v = if (70.0..96.0).contains(&x) && (36.0..62.0).contains(&y) {
                        other(x + 2.0 * tf, y - 1.5 * tf)
                    } else {
                        let p = hap(&hi, [x, y]);
                        tex(p[0], p[1])
                    };
                    data.push(v as f32);
                }
            }
            LumaFrame::new(128, 128, data).unwrap()
        })
        .collect()
}

const SPEC: PlanarSpec = PlanarSpec {
    quad: [[24.0, 24.0], [104.0, 24.0], [104.0, 104.0], [24.0, 104.0]],
    max_features: 64,
    feature_radius: 6,
    search_radius: 8,
    min_spacing_px: 8,
    ransac_iterations: 200,
    inlier_px: 0.75,
    seed: 7,
};

fn run(frames: &[LumaFrame], spec: &PlanarSpec) -> Result<Vec<PlanarFrame>, TrackError> {
    track_planar(frames, 0, TrackDirection::Forward, spec, &TrackParams::DEFAULT, &TrackLimits::DEFAULT, &CancellationToken::default(), &mut |_, _| {})
}

#[test]
fn planar_track_recovers_the_plane_through_an_occluder_deterministically() {
    let f = frames();
    let out = run(&f, &SPEC).unwrap();
    assert_eq!(out.len(), 8);
    let mut worst = 0.0f64;
    let mut rejected = 0usize;
    for pf in &out {
        assert_eq!(pf.status, PlanarStatus::Tracked, "{pf:?}");
        let truth = ht(pf.frame as f64);
        for (c, q) in SPEC.quad.iter().zip(pf.quad) {
            let e = hap(&truth, *c);
            worst = worst.max(((e[0] - q[0]).powi(2) + (e[1] - q[1]).powi(2)).sqrt());
        }
        rejected += pf.features - pf.inliers;
        println!("frame {}: features {}, inliers {}, worst so far {worst:.5}", pf.frame, pf.features, pf.inliers);
    }
    println!("planar: worst quad corner error {worst:.5} px, features {} at start, {rejected} matches rejected over the run", out[0].features);
    assert!(out[0].features >= 20, "features detected: {}", out[0].features);
    assert!(rejected > 0, "the occluder's matches are rejected as outliers");
    assert!(worst < 0.1, "quad corner bound 0.1 px, measured {worst}");
    let again = run(&f, &SPEC).unwrap();
    let bits = |v: &[PlanarFrame]| -> Vec<u64> { v.iter().flat_map(|p| p.homography.m.map(f64::to_bits)).collect() };
    assert_eq!(bits(&out), bits(&again), "same seed, same bits");
}

#[test]
fn planar_hostile_specs_and_textureless_planes_are_refused_typed() {
    let f: Vec<LumaFrame> = frames().into_iter().take(2).collect();
    let bad = |spec: PlanarSpec| run(&f, &spec).unwrap_err();
    let bowtie = [[24.0, 24.0], [104.0, 104.0], [104.0, 24.0], [24.0, 104.0]];
    assert_eq!(bad(PlanarSpec { quad: bowtie, ..SPEC }), TrackError::Parameter, "non-convex quad");
    let line = [[0.0, 0.0], [10.0, 10.0], [20.0, 20.0], [30.0, 30.0]];
    assert_eq!(bad(PlanarSpec { quad: line, ..SPEC }), TrackError::Parameter, "collinear quad");
    assert_eq!(bad(PlanarSpec { quad: [[f64::NAN, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]], ..SPEC }), TrackError::Parameter);
    assert_eq!(bad(PlanarSpec { max_features: 3, ..SPEC }), TrackError::Parameter);
    assert_eq!(bad(PlanarSpec { max_features: 1_000_000, ..SPEC }), TrackError::Parameter);
    assert_eq!(bad(PlanarSpec { ransac_iterations: 0, ..SPEC }), TrackError::Parameter);
    assert_eq!(bad(PlanarSpec { inlier_px: f64::NAN, ..SPEC }), TrackError::Parameter);
    assert_eq!(bad(PlanarSpec { search_radius: 1_000_000, ..SPEC }), TrackError::Parameter, "huge search region");
    let tight = TrackLimits { max_work: 1_000_000, ..TrackLimits::DEFAULT };
    let e = track_planar(&f, 0, TrackDirection::Forward, &SPEC, &TrackParams::DEFAULT, &tight, &CancellationToken::default(), &mut |_, _| {});
    assert!(matches!(e.unwrap_err(), TrackError::OverBudget { limit: 1_000_000, .. }));
    let flat = vec![LumaFrame::new(128, 128, vec![0.5; 128 * 128]).unwrap(); 2];
    assert_eq!(run(&flat, &SPEC).unwrap_err(), TrackError::Degenerate, "no features on a flat plane");
    let cancel = CancellationToken::default();
    cancel.cancel();
    let e = track_planar(&f, 0, TrackDirection::Forward, &SPEC, &TrackParams::DEFAULT, &TrackLimits::DEFAULT, &cancel, &mut |_, _| {});
    assert_eq!(e.unwrap_err(), TrackError::Canceled);
}
