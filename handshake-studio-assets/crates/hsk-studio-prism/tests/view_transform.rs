//! Lane SMB-PRISM-VIEW-TRANSFORM: the ACES 1.x SDR video view (RRT + ODT 100 nits dim) of
//! `hsk_studio_prism::ViewTransformId`.
//!
//! Reference values:
//! - OCIO: OpenColorIO v2.3.2 (BSD-3-Clause) `tests/cpu/transforms/BuiltinTransform_tests.cpp`,
//!   built-in "ACES-OUTPUT - ACES2065-1_to_CIE-XYZ-D65 - SDR-VIDEO_1.0" (RRT + ODT Rec709 100 nits
//!   dim up to CIE XYZ D65, an independent implementation of the same CTL): ACES2065-1
//!   (0.5, 0.4, 0.3) -> XYZ (0.34128153, 0.32533440, 0.24217427). The view's display-linear Rec.709
//!   output is taken to XYZ with the Rec.709 primaries matrix computed here from chromaticities.
//!   OCIO's own suite checks this value at 1e-6 absolute; its splines are refitted from the CTL.
//! - CTL hand values (aces-dev v1.3.1 `ACESlib.Tonescales.ctl`, `ACESlib.ODT_Common.ctl`): a
//!   neutral at the RRT mid point 0.18 lands on both spline mid points (4.8 nits), so its linear
//!   code value is ((4.8 - 0.02) / (48 - 0.02))^0.9811 on every channel, and its sRGB code value is
//!   the IEC 61966-2-1 encoding of that.
use hsk_studio_prism::{DisplayEncoding, Error, ViewTransformId, aces1_sdr_video_from_aces2065};

const VIEW: ViewTransformId = ViewTransformId::Aces1SdrVideo;

fn rec709_to_xyz(rgb: [f32; 3]) -> [f64; 3] {
    // Normalised primaries matrix of Rec.709 / sRGB (D65), from chromaticities.
    let p = [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]];
    let w = [0.3127 / 0.3290, 1.0, (1.0 - 0.3127 - 0.3290) / 0.3290];
    let col = |xy: [f64; 2]| [xy[0] / xy[1], 1.0, (1.0 - xy[0] - xy[1]) / xy[1]];
    let (r, g, b) = (col(p[0]), col(p[1]), col(p[2]));
    let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    // Solve m * s = w (Cramer).
    let det = |a: [[f64; 3]; 3]| {
        a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
            + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0])
    };
    let d = det(m);
    let mut s = [0.0; 3];
    for (k, sk) in s.iter_mut().enumerate() {
        let mut mk = m;
        for row in 0..3 {
            mk[row][k] = w[row];
        }
        *sk = det(mk) / d;
    }
    let v = rgb.map(f64::from);
    let mut xyz = [0.0; 3];
    for (row, out) in xyz.iter_mut().enumerate() {
        *out = (0..3).map(|k| m[row][k] * s[k] * v[k]).sum();
    }
    xyz
}

fn rel(a: f64, b: f64) -> f64 {
    (a - b).abs() / b.abs().max(1e-12)
}

#[test]
fn aces_sdr_view_matches_ocio_and_ctl_hand_values() {
    // OCIO SDR-VIDEO_1.0 regression value (see module docs).
    let out = aces1_sdr_video_from_aces2065([0.5, 0.4, 0.3]).expect("finite");
    let xyz = rec709_to_xyz(out);
    let ocio = [0.341_281_53, 0.325_334_40, 0.242_174_27];
    let d_ocio = (0..3).map(|i| rel(xyz[i], ocio[i])).fold(0.0, f64::max);
    println!("ocio sdr-video xyz {xyz:?} vs {ocio:?}: max relative delta {d_ocio:e}");

    // CTL hand value: mid grey.
    let lin_grey = ((4.8_f64 - 0.02) / (48.0 - 0.02)).powf(0.9811);
    let grey = aces1_sdr_video_from_aces2065([0.18; 3]).expect("finite");
    let d_grey = grey.iter().map(|&v| rel(f64::from(v), lin_grey)).fold(0.0, f64::max);
    println!("mid grey {grey:?} vs {lin_grey}: max relative delta {d_grey:e}");
    let srgb = DisplayEncoding::Srgb.encode(grey).expect("finite");
    let srgb_hand = 1.055 * lin_grey.powf(1.0 / 2.4) - 0.055;
    let d_srgb = srgb.iter().map(|&v| rel(f64::from(v), srgb_hand)).fold(0.0, f64::max);
    let bt1886 = DisplayEncoding::Bt1886.encode(grey).expect("finite");
    let d_1886 = bt1886
        .iter()
        .map(|&v| rel(f64::from(v), lin_grey.powf(1.0 / 2.4)))
        .fold(0.0, f64::max);
    println!("mid grey sRGB {srgb:?} vs {srgb_hand}: {d_srgb:e}; BT.1886 {bt1886:?}: {d_1886:e}");

    // Working-space input: a Rec.709 neutral adapts to an ACES neutral (Bradford maps white to
    // white), so the working-space view of 0.18 grey is the same mid grey.
    let w = VIEW.apply([0.18; 3]).expect("finite");
    let d_work = w.iter().map(|&v| rel(f64::from(v), lin_grey)).fold(0.0, f64::max);
    println!("working-space mid grey {w:?}: max relative delta {d_work:e}");

    // Tolerances from the first run (2026-10-15, x86_64-windows-msvc): OCIO max relative delta
    // 5.9e-7 (OCIO refits the CTL splines and runs binary32); mid grey 7.3e-8, sRGB 3.9e-8,
    // BT.1886 8.6e-9, working-space grey 7.3e-8 (binary32 rounding of the binary64 result).
    assert!(d_ocio <= 2e-6, "OCIO delta {d_ocio:e}");
    assert!(d_grey <= 2e-7, "grey delta {d_grey:e}");
    assert!(d_srgb <= 1e-7 && d_1886 <= 1e-7, "encoding delta {d_srgb:e} {d_1886:e}");
    assert!(d_work <= 2e-7, "working-space grey delta {d_work:e}");
    assert_eq!(ViewTransformId::from_key(VIEW.key()), Some(VIEW));
}

#[test]
fn aces_sdr_view_is_monotonic_with_black_and_white_points() {
    // Black: ACES 0 -> 0.0001 nits after the RRT -> below the ODT spline minimum -> 0.02 nits =
    // cinema black -> linear code value 0.
    assert_eq!(VIEW.apply([0.0; 3]).unwrap(), [0.0; 3]);
    assert_eq!(aces1_sdr_video_from_aces2065([0.0; 3]).unwrap(), [0.0; 3]);
    // White: the ODT maximum point is c5(0.18 * 2^6.5) -> 48 nits -> 1.0; above it the slope 0.04
    // extension exceeds 1 and the ODT clamps.
    let white_in = (0.18 * 2f64.powf(6.5)) as f32;
    let at_white = aces1_sdr_video_from_aces2065([white_in; 3]).unwrap();
    assert!(at_white.iter().all(|&v| (v - 1.0).abs() <= 1e-5), "{at_white:?}");
    for big in [white_in * 1.01, 1e4, 1e30, f32::MAX] {
        assert_eq!(aces1_sdr_video_from_aces2065([big; 3]).unwrap(), [1.0; 3], "{big}");
        assert_eq!(VIEW.apply([big; 3]).unwrap(), [1.0; 3], "{big}");
    }
    // Neutral ramp over 1e-6 .. 1e5 (110 steps per decade): every channel non-decreasing, inside
    // [0, 1]; a coloured ray k * (1.0, 0.5, 0.2): luminance non-decreasing.
    let y = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let mut prev = [0.0f32; 3];
    let mut prev_y = 0.0f32;
    let mut steps = 0;
    for i in 0..=1210 {
        let k = 10f32.powf(-6.0 + i as f32 / 110.0);
        let n = VIEW.apply([k; 3]).unwrap();
        for c in 0..3 {
            assert!((0.0..=1.0).contains(&n[c]), "{k} {n:?}");
            assert!(n[c] >= prev[c], "neutral not monotonic at {k}: {prev:?} -> {n:?}");
        }
        prev = n;
        let col = VIEW.apply([k, 0.5 * k, 0.2 * k]).unwrap();
        assert!(y(col) + 1e-7 >= prev_y, "colour ray luminance not monotonic at {k}");
        prev_y = y(col);
        steps += 1;
    }
    assert_eq!(steps, 1211);
    assert_eq!(prev, [1.0; 3]);
}

#[test]
fn hostile_view_input_is_typed_or_finite() {
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        for c in 0..3 {
            let mut px = [0.18f32; 3];
            px[c] = bad;
            assert_eq!(VIEW.apply(px), Err(Error::NonfiniteChannel), "{px:?}");
            assert_eq!(aces1_sdr_video_from_aces2065(px), Err(Error::NonfiniteChannel));
            assert_eq!(DisplayEncoding::Srgb.encode(px), Err(Error::NonfiniteChannel));
        }
    }
    // Negative and extreme finite input: the RRT clamps negatives; output finite in [0, 1].
    assert_eq!(VIEW.apply([-1.0; 3]).unwrap(), [0.0; 3]);
    for px in [
        [-1e30, 5.0, 0.3],
        [f32::MAX, -f32::MAX, 0.0],
        [f32::MIN_POSITIVE, 1e-45, -0.0],
        [1.0, 0.0, 0.0],
        [0.0, 0.0, 1e6],
        [0.18, 0.18, 0.18f32.next_up()],
    ] {
        let out = VIEW.apply(px).unwrap();
        assert!(out.iter().all(|v| (0.0..=1.0).contains(v)), "{px:?} -> {out:?}");
        // Deterministic: same bits on a second call.
        assert_eq!(out.map(f32::to_bits), VIEW.apply(px).unwrap().map(f32::to_bits));
    }
    // In-place form: length must be a multiple of 3; a failing triple stops with the typed error.
    let mut odd = [0.1f32; 4];
    assert_eq!(VIEW.apply_in_place(&mut odd), Err(Error::ChannelRange));
    assert_eq!(odd, [0.1; 4]);
    let mut buf = [0.18, 0.18, 0.18, f32::NAN, 0.0, 0.0];
    assert_eq!(VIEW.apply_in_place(&mut buf), Err(Error::NonfiniteChannel));
    assert_eq!(&buf[..3], &VIEW.apply([0.18; 3]).unwrap());
}
