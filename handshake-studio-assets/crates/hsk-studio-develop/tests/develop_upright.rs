//! Lane SMB-DEVELOP-LENS-PRESETS slice 2: automatic perspective correction and manual geometry
//! (STU-RAW-118). Lightroom Upright parity is UNVERIFIED (D5); these tests prove this crate's
//! documented method (src/upright.rs, src/geometry.rs) on synthetic scenes with known camera
//! rotations: corrected scene lines must come out vertical / horizontal.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;

const W: u32 = 320;
const H: u32 = 240;
const UNIT: f64 = 160.0; // half the long side
const LUMA: [f64; 3] = adjust::REC709_LUMA;

type M3 = [[f64; 3]; 3];

fn mm(a: &M3, b: &M3) -> M3 {
    std::array::from_fn(|r| std::array::from_fn(|c| a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c]))
}

fn apply(m: &[f64; 9], p: (f64, f64)) -> (f64, f64) {
    let w = m[6] * p.0 + m[7] * p.1 + m[8];
    ((m[0] * p.0 + m[1] * p.1 + m[2]) / w, (m[3] * p.0 + m[4] * p.1 + m[5]) / w)
}

fn flat(m: &M3) -> [f64; 9] {
    [m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2]]
}

/// Camera rotation homography `K R K^-1` with the analysis' default camera (35 mm, centred).
fn camera(pitch: f64, yaw: f64, roll: f64) -> [f64; 9] {
    let f = 35.0 / 43.266_615 * (f64::from(W).hypot(f64::from(H)) / UNIT);
    let k: M3 = [[f, 0.0, 0.0], [0.0, f, 0.0], [0.0, 0.0, 1.0]];
    let ki: M3 = [[1.0 / f, 0.0, 0.0], [0.0, 1.0 / f, 0.0], [0.0, 0.0, 1.0]];
    let (sp, cp) = pitch.to_radians().sin_cos();
    let (sy, cy) = yaw.to_radians().sin_cos();
    let (sr, cr) = roll.to_radians().sin_cos();
    let rx: M3 = [[1.0, 0.0, 0.0], [0.0, cp, -sp], [0.0, sp, cp]];
    let ry: M3 = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let rz: M3 = [[cr, -sr, 0.0], [sr, cr, 0.0], [0.0, 0.0, 1.0]];
    flat(&mm(&k, &mm(&rz, &mm(&ry, &mm(&rx, &ki)))))
}

fn invert(m: &[f64; 9]) -> [f64; 9] {
    let [a, b, c, d, e, f, g, h, i] = *m;
    let adj = [e * i - f * h, c * h - b * i, b * f - c * e, f * g - d * i, a * i - c * g, c * d - a * f, d * h - e * g, b * g - a * h, a * e - b * d];
    let det = a * adj[0] + b * adj[3] + c * adj[6];
    adj.map(|v| v / det)
}

const XS: [f64; 5] = [-0.7, -0.35, 0.0, 0.35, 0.7];
const YS: [f64; 3] = [-0.45, 0.0, 0.45];

/// Captured frame of a grid of scene lines (normalised coordinates) seen through `h0`.
fn render(h0: &[f64; 9]) -> Vec<f32> {
    let inv = invert(h0);
    let sigma = 1.5 / UNIT;
    let mut out = Vec::with_capacity((W * H * 3) as usize);
    for y in 0..H {
        for x in 0..W {
            let p = ((f64::from(x) + 0.5 - 160.0) / UNIT, (f64::from(y) + 0.5 - 120.0) / UNIT);
            let w = inv[6] * p.0 + inv[7] * p.1 + inv[8];
            let v = if w <= 0.0 {
                0.1
            } else {
                let q = apply(&inv, p);
                let line = |d: f64| (-(d * d) / (2.0 * sigma * sigma)).exp();
                let a = XS.iter().map(|c| line(q.0 - c)).fold(0.0, f64::max);
                let b = YS.iter().map(|c| line(q.1 - c)).fold(0.0, f64::max);
                0.1 + 0.7 * a.max(b)
            };
            out.extend_from_slice(&[v as f32; 3]);
        }
    }
    out
}

/// Angle (degrees) of the image of scene segment `a -> b` under `h0` then `h` from `axis` (0 = vertical).
fn tilt(h0: &[f64; 9], h: &[f64; 9], a: (f64, f64), b: (f64, f64), vertical: bool) -> f64 {
    let (p, q) = (apply(h, apply(h0, a)), apply(h, apply(h0, b)));
    let (dx, dy) = ((q.0 - p.0).abs(), (q.1 - p.1).abs());
    if vertical { dx.atan2(dy).to_degrees() } else { dy.atan2(dx).to_degrees() }
}

fn worst(h0: &[f64; 9], h: &[f64; 9], vertical: bool) -> f64 {
    if vertical {
        XS.iter().map(|x| tilt(h0, h, (*x, -0.4), (*x, 0.4), true)).fold(0.0, f64::max)
    } else {
        YS.iter().map(|y| tilt(h0, h, (-0.6, *y), (0.6, *y), false)).fold(0.0, f64::max)
    }
}

#[test]
fn pitched_and_rolled_scenes_are_rectified() {
    // Camera pitched up 10 degrees: verticals converge. Vertical mode makes them parallel.
    let h0 = camera(10.0, 0.0, 0.0);
    let a = analyse_upright(&render(&h0), W, H, &Geometry::default(), LUMA).unwrap();
    assert!(worst(&h0, &IDENTITY_3X3, true) > 3.0, "the scene really converges");
    let vertical = a.transforms[UprightMode::Vertical.index()];
    assert!(worst(&h0, &vertical, true) < 0.3, "vertical: {}", worst(&h0, &vertical, true));
    assert!(a.vertical_vanishing_point.is_some() && a.lines.len() >= 6, "{} lines", a.lines.len());
    assert_eq!(a.transforms[UprightMode::Off.index()], IDENTITY_3X3);
    // The principal point stays put and the area scale there is 1.
    let c = apply(&vertical, (0.0, 0.0));
    assert!(c.0.abs() < 1e-9 && c.1.abs() < 1e-9);
    // Pitch + yaw + roll: full mode squares both families.
    let h0 = camera(8.0, 6.0, 3.0);
    let a = analyse_upright(&render(&h0), W, H, &Geometry::default(), LUMA).unwrap();
    let full = a.transforms[UprightMode::Full.index()];
    let (v, hz) = (worst(&h0, &full, true), worst(&h0, &full, false));
    assert!(v < 0.5 && hz < 0.5, "full: {v} {hz}");
    // Auto picks full here (rotation well under 20 degrees).
    assert_eq!(a.transforms[UprightMode::Auto.index()], full);
    // Pure 4-degree roll: level turns the picture back.
    let h0 = camera(0.0, 0.0, 4.0);
    let a = analyse_upright(&render(&h0), W, H, &Geometry::default(), LUMA).unwrap();
    let level = a.transforms[UprightMode::Level.index()];
    assert!(worst(&h0, &level, true) < 0.3 && worst(&h0, &level, false) < 0.3);
}

#[test]
fn guided_mode_follows_the_guides_exactly() {
    let h0 = camera(12.0, 0.0, 0.0);
    let to_unit = |p: (f64, f64)| [(p.0 * UNIT + 160.0) / f64::from(W), (p.1 * UNIT + 120.0) / f64::from(H)];
    // Scene segments whose captured images lie inside the frame (this pitch moves points up).
    let guide = |x: f64| [to_unit(apply(&h0, (x, 0.0))), to_unit(apply(&h0, (x, 0.6)))];
    let geometry = Geometry { upright_guides: vec![guide(-0.7), guide(0.7)], ..Geometry::default() };
    // A flat frame: the guides are the only evidence.
    let flat_frame = vec![0.3f32; (W * H * 3) as usize];
    let a = analyse_upright(&flat_frame, W, H, &geometry, LUMA).unwrap();
    assert!(a.lines.is_empty());
    for mode in [UprightMode::Auto, UprightMode::Level, UprightMode::Vertical, UprightMode::Full] {
        assert_eq!(a.transforms[mode.index()], IDENTITY_3X3, "{mode:?} without evidence is the identity");
    }
    let guided = a.transforms[UprightMode::Guided.index()];
    assert!(worst(&h0, &guided, true) < 0.01, "guided: {}", worst(&h0, &guided, true));
}

#[test]
fn manual_transform_matches_hand_values() {
    let cancel = CancellationToken::default();
    let on = Enables { enable_transform: true, ..Enables::all_disabled() };
    let ramp: Vec<f32> = (0..H).flat_map(|y| (0..W).flat_map(move |x| [(f64::from(x) + 0.5) as f32 / 320.0, (f64::from(y) + 0.5) as f32 / 240.0, 0.5])).collect();
    let mut a = Adjustments::default();
    a.geometry.perspective_rotate = 10.0;
    let (out, report) = apply_geometry(&ramp, W, H, &a, &on, &NormalizedCrop::full(), Threads::single(), &cancel).unwrap();
    let (s, c) = 10f64.to_radians().sin_cos();
    let expect = [c, -s, 0.0, s, c, 0.0, 0.0, 0.0, 1.0];
    assert!(report.transform.iter().zip(expect).all(|(x, y)| (x - y).abs() < 1e-12), "{:?}", report.transform);
    // Output pixel (200, 150) samples the source at the inverse rotation about the centre; the ramp
    // is affine, so Catmull-Rom reproduces it exactly away from the frame edge.
    let (u, v) = ((200.5 - 160.0) / UNIT, (150.5 - 120.0) / UNIT);
    let (sx, sy) = (c * u + s * v, -s * u + c * v);
    let at = |x: usize, y: usize, ch: usize| out[(y * W as usize + x) * 3 + ch];
    assert!((f64::from(at(200, 150, 0)) - (sx * UNIT + 160.0) / 320.0).abs() < 1e-5);
    assert!((f64::from(at(200, 150, 1)) - (sy * UNIT + 120.0) / 240.0).abs() < 1e-5);
    // Vertical +50: homogeneous row (0, 0.2, 1); scale 120 and aspect 0 compose after it.
    let mut a = Adjustments::default();
    a.geometry.perspective_vertical = 50.0;
    a.geometry.perspective_scale = 120.0;
    let (_, report) = apply_geometry(&ramp, W, H, &a, &on, &NormalizedCrop::full(), Threads::single(), &cancel).unwrap();
    let t = report.transform;
    assert!((t[7] - 0.2).abs() < 1e-12 && (t[0] - 1.2).abs() < 1e-12 && (t[4] - 1.2).abs() < 1e-12 && t[6] == 0.0);
    // The group off: identity, no report transform.
    let off = apply_geometry(&ramp, W, H, &a, &Enables::all_disabled(), &NormalizedCrop::full(), Threads::single(), &cancel).unwrap();
    assert_eq!((off.0, off.1.transform), (ramp.clone(), IDENTITY_3X3));
}

#[test]
fn develop_runs_upright_whole_frame_from_stored_or_analysed_candidates() {
    let plane = CfaPlane::with_limits(
        PlaneInit {
            width: 64,
            height: 48,
            stride_bytes: 128,
            sensor_origin: (0, 0),
            cfa: Cfa { pattern: CfaPattern::Bayer(Bayer::Rggb), phase_x: 0, phase_y: 0 },
            black: [0; 3],
            white: [4095; 3],
            source_revision: 1,
            bytes: (0..64u32 * 48).flat_map(|i| (if (i % 64) % 16 < 2 { 3000u16 } else { 400 }).to_le_bytes()).collect(),
        },
        Limits::LARGE,
    )
    .unwrap();
    let cancel = CancellationToken::default();
    let profile = ProfileBinding { profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(), sha256: [7; 32] };
    let mut r = DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile);
    r.enables.enable_transform = true;
    r.adjust.geometry.upright_mode = UprightMode::Vertical;
    let (_, receipt) = develop_to_image(&plane, &r, 1, &cancel).unwrap();
    let g = receipt.geometry.unwrap();
    assert_eq!(g.upright_mode, UprightMode::Vertical);
    let analysed = g.upright_transforms.unwrap();
    // Stored candidates are used verbatim (no re-analysis): a stored keystone is the transform.
    let mut stored = analysed;
    stored[UprightMode::Vertical.index()] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.1, 1.0];
    r.adjust.geometry.upright_transforms = Some(stored);
    let (img, receipt) = develop_to_image(&plane, &r, 1, &cancel).unwrap();
    assert_eq!(receipt.geometry.as_ref().unwrap().transform, stored[3]);
    let again = develop_to_image_threads(&plane, &r, 1, Threads::new(3).unwrap(), &cancel).unwrap().0;
    assert_eq!(img, again, "thread invariant");
    let tiles = develop(
        &DevelopRequest {
            plane: &plane,
            recipe: &r,
            recipe_revision: 1,
            expected_source_revision: 1,
            grid: hsk_studio_pigment::Grid { origin_x: 0, origin_y: 0, tile_width: 32, tile_height: 32 },
            output_origin: (0, 0),
            execution_hint: ExecutionHint::Auto,
        },
        &cancel,
        &mut |_| Ok(()),
    );
    assert_eq!(tiles.err(), Some(DevelopError::UnsupportedContribution("transform_whole_frame")));
}

#[test]
fn hostile_geometry_is_refused_typed() {
    let cancel = CancellationToken::default();
    let on = Enables { enable_transform: true, ..Enables::all_disabled() };
    let img = vec![0.2f32; 32 * 32 * 3];
    let run = |f: &dyn Fn(&mut Geometry)| {
        let mut a = Adjustments::default();
        f(&mut a.geometry);
        apply_geometry(&img, 32, 32, &a, &on, &NormalizedCrop::full(), Threads::single(), &cancel).err()
    };
    assert_eq!(run(&|g| g.perspective_rotate = 15.0), Some(DevelopError::InvalidAdjustment));
    assert_eq!(run(&|g| g.perspective_scale = 200.0), Some(DevelopError::InvalidAdjustment));
    assert_eq!(run(&|g| g.perspective_vertical = f64::NAN), Some(DevelopError::NonFinite));
    assert_eq!(run(&|g| g.upright_guides = vec![[[0.1, 0.1], [0.1, 0.9]]; 5]), Some(DevelopError::InvalidAdjustment));
    assert_eq!(run(&|g| g.upright_guides = vec![[[0.5, 0.5], [0.5, 0.5]]]), Some(DevelopError::InvalidAdjustment));
    assert_eq!(run(&|g| g.upright_focal_length_35mm = -1.0), Some(DevelopError::InvalidAdjustment));
    // Stored candidates: singular, non-finite, and one that folds the frame (horizon inside it).
    let singular = |g: &mut Geometry| {
        g.upright_mode = UprightMode::Full;
        g.upright_transforms = Some([[0.0; 9]; 6]);
    };
    assert_eq!(run(&singular), Some(DevelopError::InvalidTransform));
    let folding = |g: &mut Geometry| {
        g.upright_mode = UprightMode::Full;
        let mut t = [IDENTITY_3X3; 6];
        t[4] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 3.0, 1.0];
        g.upright_transforms = Some(t);
    };
    assert_eq!(run(&folding), Some(DevelopError::InvalidTransform));
    let nan = |g: &mut Geometry| {
        let mut t = [IDENTITY_3X3; 6];
        t[1][2] = f64::INFINITY;
        g.upright_transforms = Some(t);
    };
    assert_eq!(run(&nan), Some(DevelopError::NonFinite));
    // Analysis inputs.
    assert_eq!(analyse_upright(&img, 32, 31, &Geometry::default(), LUMA).err(), Some(DevelopError::InvalidDimensions));
    let mut bad = img.clone();
    bad[7] = f32::NAN;
    assert_eq!(analyse_upright(&bad, 32, 32, &Geometry::default(), LUMA).err(), Some(DevelopError::NonFinite));
    assert_eq!(analyse_upright(&[], 0, 0, &Geometry::default(), LUMA).err(), Some(DevelopError::InvalidDimensions));
}
