//! Lane SMB-DEVELOP-LENS-PRESETS slice 1: lens corrections (STU-RAW-117 optics, STU-RAW-127 lens
//! profile model). Lightroom-matching values are UNVERIFIED (D5); these tests prove this crate's
//! documented model (src/optics.rs, src/lens_profile.rs) against hand-computed values.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_develop::*;

fn profile() -> ProfileBinding {
    ProfileBinding { profile_id: DomainId::parse("SCPF-019abcde-0000-7000-8000-000000000001").unwrap(), sha256: [7; 32] }
}

fn lens_on() -> Enables {
    Enables { enable_lens_corrections: true, ..Enables::all_disabled() }
}

fn model(k1: f64) -> LensModel {
    LensModel {
        perspective: PerspectiveModel { focal_x: 1.0, focal_y: 1.0, centre_x: 0.5, centre_y: 0.5, radial: [k1, 0.0, 0.0], tangential: [0.0; 2], scale: 1.0 },
        vignette: None,
        ca_green: None,
        ca_red_green: None,
        ca_blue_green: None,
        residual: None,
    }
}

fn binding(model: LensModel) -> LensBinding {
    LensBinding { name: "test".into(), sha256: [1; 32], focal_mm: 24.0, aperture_value: None, distance_m: None, samples_used: 1, model }
}

fn with_profile(model: LensModel) -> Adjustments {
    let mut a = Adjustments::default();
    a.optics.profile = Some(binding(model));
    a
}

fn run(rgb: &[f32], w: u32, h: u32, adjust: &Adjustments) -> Result<(Vec<f32>, GeometryReport), DevelopError> {
    apply_geometry(rgb, w, h, adjust, &lens_on(), &NormalizedCrop::full(), Threads::single(), &CancellationToken::default())
}

const PROFILE: &str = "hsk-lens-profile 1
# two focal lengths, two apertures at 24 mm
name Test Zoom 24-70
projection rectilinear
sample 24 3 inf
perspective 0.8 0.8 0.5 0.5 -0.10 0.02 0
vignette -0.40 0.1 0
residual 0.01 0.002
sample 24 5 inf
perspective 0.8 0.8 0.5 0.5 -0.10 0.02 0
vignette -0.20 0.1 0
residual 0.03 0.004
sample 70 3 1e32
perspective 2.0 2.0 0.5 0.5 0.04 0 0 0 0 1.0
ca_red_green 1.002 0 0 0
";

#[test]
fn profile_parse_and_bind_interpolate_the_sampled_grid() {
    let p = LensProfile::parse(PROFILE.as_bytes()).unwrap();
    assert_eq!((p.name.as_str(), p.projection, p.samples.len()), ("Test Zoom 24-70", LensProjection::Rectilinear, 3));
    assert_eq!(p.samples[2].distance_m, Some(f64::INFINITY), "1e32 is the infinity sentinel");
    // 24 mm, Av 4 = halfway between Av 3 and 5: vignette a1 = (-0.4 + -0.2) / 2, residual blended.
    let b = p.bind(24.0, Some(4.0), None).unwrap();
    assert!((b.model.vignette.unwrap()[0] + 0.30).abs() < 1e-12);
    let (mean, sd) = b.model.residual.unwrap();
    assert!((mean - 0.02).abs() < 1e-12 && (sd - 0.003).abs() < 1e-12);
    assert_eq!(b.samples_used, 2);
    assert_eq!(b.sha256, p.sha256);
    // 47 mm at Av 3: t = (47 - 24) / 46 = 0.5 between 24 and 70 -> k1 = (-0.10 + 0.04) / 2, focal 1.4,
    // the CA model of the 70 mm sample alone (absent at 24 mm).
    let b = p.bind(47.0, Some(3.0), Some(f64::INFINITY)).unwrap();
    let q = b.model.perspective;
    assert!((q.radial[0] + 0.03).abs() < 1e-12 && (q.focal_x - 1.4).abs() < 1e-12 && (q.radial[1] - 0.01).abs() < 1e-12);
    assert_eq!(b.model.ca_red_green, Some(ChannelModel { scale: 1.002, radial: [0.0; 3] }));
    // Outside the grid clamps to the end sample.
    assert_eq!(p.bind(200.0, Some(3.0), None).unwrap().model.perspective.focal_x, 2.0);
    // A fisheye profile is never evaluated with the rectilinear model.
    let fish = PROFILE.replace("projection rectilinear", "projection fisheye");
    assert_eq!(
        LensProfile::parse(fish.as_bytes()).unwrap().bind(24.0, None, None).err(),
        Some(DevelopError::UnsupportedContribution("fisheye_lens_model"))
    );
}

/// Ideal scene: vertical Gaussian lines (sigma 1.2 px) on 0.1 grey.
const LINES: [f64; 5] = [20.0, 50.0, 80.0, 110.0, 140.0];
fn scene(x: f64) -> f64 {
    0.1 + 0.8 * LINES.iter().map(|c| (-(x - c).powi(2) / (2.0 * 1.44)).exp()).sum::<f64>()
}

fn centroid(img: &[f32], w: usize, y: usize, around: f64) -> f64 {
    let (mut sum, mut weight) = (0.0, 0.0);
    for x in (around as usize - 6)..=(around as usize + 6) {
        let v = f64::from(img[(y * w + x) * 3 + 1]) - 0.1;
        sum += v * (x as f64 + 0.5);
        weight += v;
    }
    sum / weight
}

#[test]
fn known_barrel_grid_is_straightened() {
    // Captured frame of the line scene through barrel distortion k1 = -0.25 (focal = long side): each
    // captured pixel shows the ideal point u with u * R(u) = captured (fixed-point inversion).
    let (w, h) = (160usize, 120usize);
    let (cx, cy, long) = (80.0, 60.0, 160.0);
    let k1 = -0.25;
    let mut captured = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = ((x as f64 + 0.5 - cx) / long, (y as f64 + 0.5 - cy) / long);
            let (mut u, mut v) = (dx, dy);
            for _ in 0..60 {
                let r = 1.0 + k1 * (u * u + v * v);
                (u, v) = (dx / r, dy / r);
            }
            let s = scene(cx + u * long) as f32;
            captured.extend_from_slice(&[s, s, s]);
        }
    }
    // Uncorrected, the outer line bows by more than 2 px between the top row and the centre row.
    assert!((centroid(&captured, w, 5, 22.0) - centroid(&captured, w, 60, 20.0)).abs() > 2.0);
    let (out, report) = run(&captured, w as u32, h as u32, &with_profile(model(k1))).unwrap();
    assert_eq!(report.outside_pixels, 0);
    assert_eq!(report.lens_profile, Some(("test".into(), [1; 32])));
    for line in LINES {
        for y in [5usize, 30, 60, 90, 114] {
            let c = centroid(&out, w, y, line);
            assert!((c - line).abs() < 0.25, "line {line} row {y}: {c}");
        }
    }
    // Distortion scale 0 disables the profile geometry (vignette and CA would still run): identity.
    let mut off = with_profile(model(k1));
    off.optics.lens_profile_distortion_scale = 0.0;
    assert_eq!(run(&captured, w as u32, h as u32, &off).unwrap().0, captured);
}

#[test]
fn vignetting_and_lateral_ca_match_hand_values() {
    let (w, h) = (200u32, 100u32);
    let flat = vec![0.5f32; (w * h * 3) as usize];
    let at = |img: &[f32], x: u32, y: u32, c: usize| img[((y * w + x) * 3) as usize + c];
    // Profile falloff V = 1 - 0.3 r^2 at the corner pixel (0, 0): u = -99.5/200, v = -49.5/200.
    let mut m = model(0.0);
    m.vignette = Some([-0.3, 0.0, 0.0]);
    let r2 = (99.5f64 / 200.0).powi(2) + (49.5f64 / 200.0).powi(2);
    let v = 1.0 - 0.3 * r2;
    let (out, _) = run(&flat, w, h, &with_profile(m)).unwrap();
    assert!((f64::from(at(&out, 0, 0, 1)) - 0.5 / v).abs() < 1e-6, "{}", at(&out, 0, 0, 1));
    let mut half = with_profile(m);
    half.optics.lens_profile_vignetting_scale = 50.0;
    let (out, _) = run(&flat, w, h, &half).unwrap();
    assert!((f64::from(at(&out, 0, 0, 1)) - 0.5 * v.powf(-0.5)).abs() < 1e-6);
    // Manual vignette +100 with no profile: gain 2^(q^2), q = corner distance / half diagonal.
    let mut manual = Adjustments::default();
    manual.optics.manual_vignette_amount = 100.0;
    let q2 = (99.5f64.powi(2) + 49.5f64.powi(2)) / (100.0f64.powi(2) + 50.0f64.powi(2));
    let (out, _) = run(&flat, w, h, &manual).unwrap();
    assert!((f64::from(at(&out, 0, 0, 0)) - 0.5 * q2.exp2()).abs() < 1e-6);
    // Lateral CA: a horizontal ramp (value = x / 200). Red samples 1 % farther from the centre than
    // green, blue 1 % nearer; Catmull-Rom reproduces a ramp exactly away from the edges.
    let ramp: Vec<f32> = (0..h).flat_map(|_| (0..w).flat_map(|x| [(f64::from(x) + 0.5) as f32 / 200.0; 3])).collect();
    let mut ca = model(0.0);
    ca.ca_red_green = Some(ChannelModel { scale: 1.01, radial: [0.0; 3] });
    ca.ca_blue_green = Some(ChannelModel { scale: 0.99, radial: [0.0; 3] });
    let (out, _) = run(&ramp, w, h, &with_profile(ca)).unwrap();
    let expect = |f: f64| ((100.0 + 50.5 * f) / 200.0) as f32;
    assert!((at(&out, 150, 50, 0) - expect(1.01)).abs() < 1e-6);
    assert!((at(&out, 150, 50, 1) - at(&ramp, 150, 50, 1)).abs() < 1e-6);
    assert!((at(&out, 150, 50, 2) - expect(0.99)).abs() < 1e-6);
    // auto_lateral_chromatic_aberration off: the CA models do not run.
    let mut no_ca = with_profile(ca);
    no_ca.optics.auto_lateral_chromatic_aberration = false;
    let (out, _) = run(&ramp, w, h, &no_ca).unwrap();
    assert!((at(&out, 150, 50, 0) - at(&ramp, 150, 50, 0)).abs() < 1e-6);
}

#[test]
fn lens_and_crop_angle_run_whole_frame_only_and_identity_is_exact() {
    let plane = CfaPlane::with_limits(
        PlaneInit {
            width: 48,
            height: 32,
            stride_bytes: 96,
            sensor_origin: (0, 0),
            cfa: Cfa { pattern: CfaPattern::Bayer(Bayer::Rggb), phase_x: 0, phase_y: 0 },
            black: [0; 3],
            white: [4095; 3],
            source_revision: 1,
            bytes: (0..48u32 * 32).flat_map(|i| (500 + (i % 48) as u16 * 40).to_le_bytes()).collect(),
        },
        Limits::LARGE,
    )
    .unwrap();
    let cancel = CancellationToken::default();
    let base = DevelopRecipe::native([1.0; 3], [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], profile());
    let plain = develop_to_image(&plane, &base, 1, &cancel).unwrap();
    // The group on with identity optics (no profile, zero sliders) changes nothing.
    let mut on = base.clone();
    on.enables.enable_lens_corrections = true;
    let same = develop_to_image(&plane, &on, 1, &cancel).unwrap();
    assert_eq!((same.0, same.1.geometry), (plain.0.clone(), None));
    // Manual distortion runs in the whole-frame image API (geometry report present) and moves pixels.
    let mut warped = on.clone();
    warped.adjust.optics.manual_distortion_amount = 60.0;
    warped.exposure_stops = 0.5;
    let (img, receipt) = develop_to_image(&plane, &warped, 1, &cancel).unwrap();
    assert!(receipt.geometry.is_some() && img.rgb != plain.0.rgb);
    assert_eq!((img.width, img.height, receipt.pixels), (48, 32, 48 * 32));
    // ...and is a typed refusal in the tile API, like the crop straighten angle.
    let tiles = |r: &DevelopRecipe| {
        develop(
            &DevelopRequest {
                plane: &plane,
                recipe: r,
                recipe_revision: 1,
                expected_source_revision: 1,
                grid: hsk_studio_pigment::Grid { origin_x: 0, origin_y: 0, tile_width: 16, tile_height: 16 },
                output_origin: (0, 0),
                execution_hint: ExecutionHint::Auto,
            },
            &cancel,
            &mut |_| Ok(()),
        )
        .err()
    };
    assert_eq!(tiles(&warped), Some(DevelopError::UnsupportedContribution("lens_corrections_whole_frame")));
    let mut angled = base.clone();
    angled.crop.angle = 3.0;
    assert_eq!(tiles(&angled), Some(DevelopError::UnsupportedContribution("transform_whole_frame")));
    // Crop angle 3 degrees with constrain_to_warp: zoom > 1 and no pixel reads outside the frame.
    angled.crop.constrain_to_warp = true;
    let (img, receipt) = develop_to_image(&plane, &angled, 1, &cancel).unwrap();
    let g = receipt.geometry.unwrap();
    assert!(g.constrain_zoom > 1.0 && g.outside_pixels == 0, "{g:?}");
    assert_eq!(img.rgb.len(), 48 * 32 * 3);
    // Deterministic for every thread count.
    let threads = |n| develop_to_image_threads(&plane, &warped, 1, Threads::new(n).unwrap(), &cancel).unwrap().0;
    assert_eq!(threads(1), threads(4));
}

#[test]
fn hostile_profiles_and_values_are_typed_refusals() {
    let bad = |text: &str| LensProfile::parse(text.as_bytes()).err();
    let invalid = Some(DevelopError::InvalidLensProfile);
    assert_eq!(LensProfile::parse(&vec![b'#'; MAX_LENS_PROFILE_BYTES + 1]).err(), invalid, "over the byte cap");
    assert_eq!(LensProfile::parse(&[0xff, 0xfe, 0x00]).err(), invalid, "not UTF-8");
    assert_eq!(bad("hsk-lens-profile 2\nname x\nsample 24 - -\nperspective 1 1 .5 .5 0 0 0\n"), invalid);
    assert_eq!(bad("hsk-lens-profile 1\nname x\nsample 24 - -\nperspective 1 1 .5 .5 NaN 0 0\n"), invalid);
    assert_eq!(bad("hsk-lens-profile 1\nname x\nsample 24 - -\nperspective 1 1 .5 .5 1e300 0 0\n"), invalid);
    assert_eq!(bad("hsk-lens-profile 1\nname x\nsample 24 - -\nvignette 0 0 0\n"), invalid, "no perspective");
    assert_eq!(bad("hsk-lens-profile 1\nname x\nsample 24 - -\nperspective 1 1 .5 .5 0 0 0\nperspective 1 1 .5 .5 0 0 0\n"), invalid);
    assert_eq!(bad("hsk-lens-profile 1\nname x\nsample 24 - -\nperspective 1 1 .5 .5 0 0 0\nsample 24 - -\nperspective 1 1 .5 .5 0 0 0\n"), invalid);
    assert_eq!(bad("hsk-lens-profile 1\nname x\nsample 24 - -\nperspective 1 1 1.5 .5 0 0 0\n"), invalid, "centre outside 0..1");
    assert_eq!(bad("hsk-lens-profile 1\nname x\nsample 24 - -\nperspective 1 1 .5 .5 0 0 0\nmystery 1\n"), invalid);
    assert_eq!(bad(&format!("hsk-lens-profile 1\nname {}\n", "x".repeat(2000))), invalid, "line too long");
    assert_eq!(bad("hsk-lens-profile 1\nname x\n"), invalid, "no samples");
    // Values: scales, defringe windows, NaN; a falloff that reaches 0 inside the frame.
    let flat = vec![0.5f32; 16 * 16 * 3];
    let mut a = Adjustments::default();
    a.optics.lens_profile_distortion_scale = 250.0;
    assert_eq!(run(&flat, 16, 16, &a).err(), Some(DevelopError::InvalidAdjustment));
    let mut a = Adjustments::default();
    a.optics.defringe.purple_hue_low = 80.0;
    assert_eq!(run(&flat, 16, 16, &a).err(), Some(DevelopError::InvalidAdjustment));
    let mut a = Adjustments::default();
    a.optics.manual_distortion_amount = f64::NAN;
    assert_eq!(run(&flat, 16, 16, &a).err(), Some(DevelopError::NonFinite));
    let mut m = model(0.0);
    m.vignette = Some([-20.0, 0.0, 0.0]);
    assert_eq!(run(&flat, 16, 16, &with_profile(m)).err(), invalid);
    let mut m = model(0.0);
    m.perspective.focal_x = 0.0;
    assert_eq!(run(&flat, 16, 16, &with_profile(m)).err(), invalid);
    let mut nan = flat.clone();
    nan[5] = f32::NAN;
    assert_eq!(run(&nan, 16, 16, &Adjustments::default()).err(), Some(DevelopError::NonFinite));
}
