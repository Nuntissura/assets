//! Lane SMB-PREMIERE-COLOR-TITLES slice 1: the `.cube` export added to Prism's LUT resource of lane
//! SMB-DEVELOP-LENS-PRESETS (STU-COL-159 "Studio MUST export the text cube format"; parsing and its
//! hostile-input bounds are that lane's, proven by its tests) and the clip-grade maths (STU-COL-222
//! wheels, STU-COL-227 key, STU-COL-228 vignette) against hand values.
use hsk_studio_prism::grade::{
    KeyRange, SecondaryKey, Vignette, WheelBand, Wheels, apply_wheels, band_weights, key_weight, vignette_gain,
};
use hsk_studio_prism::parse_cube;

fn close(a: [f32; 3], b: [f32; 3], tol: f32) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= tol)
}
fn close64(a: [f64; 3], b: [f64; 3]) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= 1e-9)
}

#[test]
fn cube_export_round_trips_and_evaluates_hand_values() {
    // 3D side 2, channel 0 = r*g*b (only the (1,1,1) corner is 1): tetrahedral centre value
    // (1 - 0.5) c000 + 0.5 c111 = 0.5 (trilinear would give 0.125).
    let text = "# comment\nTITLE \"product\"\nLUT_3D_SIZE 2\n\
        0 0 0\n0 0 0\n0 1 0\n0 1 0\n0 0 1\n0 0 1\n0 1 1\n1 1 1\n";
    let lut = parse_cube(text.as_bytes()).unwrap();
    assert!(close64(lut.apply([0.5, 0.5, 0.5]), [0.5, 0.5, 0.5]));
    let again = parse_cube(lut.write_cube().as_bytes()).unwrap();
    assert_eq!((&again.lut_3d, again.size_3d, &again.title), (&lut.lut_3d, 2, &lut.title));
    assert_eq!((again.domain_min, again.domain_max), ([0.0; 3], [1.0; 3]));

    // A 0..2 domain is written and read back (never clamped to 0..1); a 1D shaper keeps its rows
    // and its order before the 3D table.
    let text = "LUT_1D_SIZE 3\nLUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 2 2 2\n\
        0 0 0\n0.25 0.5 0.75\n1 1 1\n\
        0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n";
    let lut = parse_cube(text.as_bytes()).unwrap();
    let out = lut.write_cube();
    assert!(out.starts_with("LUT_1D_SIZE 3\nLUT_3D_SIZE 2\nDOMAIN_MIN 0.0 0.0 0.0\nDOMAIN_MAX 2.0 2.0 2.0\n0.0 0.0 0.0\n0.25 0.5 0.75\n"), "{out}");
    let again = parse_cube(out.as_bytes()).unwrap();
    assert_eq!((&again.lut_1d, &again.lut_3d, again.domain_max), (&lut.lut_1d, &lut.lut_3d, [2.0; 3]));
    // Input 0.5 of 0..2 -> 0.25 -> shaper halfway rows 0..1 -> (0.125, 0.25, 0.375) -> identity cube.
    assert!(close64(again.apply([0.5; 3]), [0.125, 0.25, 0.375]));

    // Hostile title: a quote or control character is dropped, so the export always parses.
    let mut odd = parse_cube(b"LUT_1D_SIZE 2\n0 0 0\n1 1 1\n").unwrap();
    odd.title = Some("a\"b\nc".into());
    let back = parse_cube(odd.write_cube().as_bytes()).unwrap();
    assert_eq!(back.title.as_deref(), Some("abc"));
}

#[test]
fn wheels_key_and_vignette_follow_the_grade_maths() {
    let c = [0.5f32, 0.25, 0.75];
    // Identity.
    assert!(close(apply_wheels(&Wheels::default(), c), c, 1e-6));
    // Temperature 0.5: TempGain = (2^0.5, 1, 2^-0.5); offset' = TempGain * (0 + P - P) = 0.
    let w = Wheels { temperature: 0.5, ..Wheels::default() };
    assert!(close(apply_wheels(&w, c), [0.5 * 2f32.sqrt(), 0.25, 0.75 / 2f32.sqrt()], 1e-6));
    // Magenta 1: TempGain = (2^0.5, 2^-1, 2^0.5).
    let w = Wheels { magenta: 1.0, ..Wheels::default() };
    assert!(close(apply_wheels(&w, c), [0.5 * 2f32.sqrt(), 0.125, 0.75 * 2f32.sqrt()], 1e-6));
    // Contrast 1 about pivot 0.5: 2c - 0.5, negatives held at 0.
    let w = Wheels { contrast: 1.0, ..Wheels::default() };
    assert!(close(apply_wheels(&w, c), [0.5, 0.0, 1.0], 1e-6));
    // Overall gamma (1, 0, 0): gamma' = 3 on red, out = 0.5^(1/3).
    let w = Wheels {
        overall: WheelBand { gamma: [1.0, 0.0, 0.0], ..WheelBand::default() },
        ..Wheels::default()
    };
    assert!(close(apply_wheels(&w, c), [0.5f32.powf(1.0 / 3.0), 0.25, 0.75], 1e-6));
    // Band weights at the defaults are the quadratic Bernstein basis.
    let [s, m, h] = band_weights(0.2, 0.5, 0.5);
    assert!((s - 0.64).abs() < 1e-6 && (m - 0.32).abs() < 1e-6 && (h - 0.04).abs() < 1e-6);
    // Shadow offset (0.1, 0, 0) on grey 0.2 adds 0.64 * 0.1 to red.
    let w = Wheels {
        shadows: WheelBand { offset: [0.1, 0.0, 0.0], ..WheelBand::default() },
        ..Wheels::default()
    };
    assert!(close(apply_wheels(&w, [0.2; 3]), [0.264, 0.2, 0.2], 1e-6));
    // Input saturation 0 collapses to the Rec.709 luma.
    let w = Wheels { input_saturation: 0.0, ..Wheels::default() };
    let y = 0.2126 * 0.5 + 0.7152 * 0.25 + 0.0722 * 0.75;
    assert!(close(apply_wheels(&w, c), [y; 3], 1e-6));
    // Hard bounds: contrast below -1 and saturation above 2 are refused, not clamped.
    assert_eq!(Wheels { contrast: -1.5, ..Wheels::default() }.validate().unwrap_err().0, "wheel.contrast");
    assert_eq!(
        Wheels { input_saturation: 2.5, ..Wheels::default() }.validate().unwrap_err().0,
        "wheel.input_saturation"
    );
    assert_eq!(Wheels { pivot: f32::NAN, ..Wheels::default() }.validate().unwrap_err().0, "wheel.pivot");

    // STU-COL-227 key weight and STU-COL-228 vignette gain.
    let key = SecondaryKey {
        hue: KeyRange { lo: 100.0, hi: 140.0, soft: 20.0 },
        saturation: KeyRange { lo: 0.2, hi: 1.0, soft: 0.1 },
        luminance: KeyRange::ALL,
        invert: false,
    };
    key.validate().unwrap();
    assert!((key_weight(&key, [120.0, 0.5, 0.5]) - 1.0).abs() < 1e-6);
    // Hue 10 degrees outside (0.5) times saturation 0.05 below (0.5).
    assert!((key_weight(&key, [150.0, 0.15, 0.5]) - 0.25).abs() < 1e-6);
    assert!(key_weight(&key, [300.0, 0.5, 0.5]).abs() < 1e-6);
    let inv = SecondaryKey { invert: true, ..key };
    assert!((key_weight(&inv, [150.0, 0.15, 0.5]) - 0.75).abs() < 1e-6);
    // A hue range through 0 (350..10) and its circular soft edge.
    let wrap = SecondaryKey { hue: KeyRange { lo: 350.0, hi: 10.0, soft: 20.0 }, saturation: KeyRange::ALL, ..key };
    assert!((key_weight(&wrap, [5.0, 0.5, 0.5]) - 1.0).abs() < 1e-6);
    assert!((key_weight(&wrap, [20.0, 0.5, 0.5]) - 0.5).abs() < 1e-6);
    assert!((key_weight(&wrap, [340.0, 0.5, 0.5]) - 0.5).abs() < 1e-6);
    assert!(SecondaryKey { hue: KeyRange { lo: 400.0, hi: 10.0, soft: 0.0 }, ..key }.validate().is_err());

    let hard = Vignette { amount: -1.0, midpoint: 50.0, roundness: 0.0, feather: 0.0 };
    hard.validate().unwrap();
    assert!((vignette_gain(&hard, 50.0, 50.0, 100.0, 100.0) - 1.0).abs() < 1e-6);
    assert!((vignette_gain(&hard, 0.5, 0.5, 100.0, 100.0) - 0.5).abs() < 1e-6);
    // Feather 100: smoothstep(0, 1, 0.5) = 0.5 at d = 0.5 -> 2^-0.5.
    let soft = Vignette { feather: 100.0, ..hard };
    assert!((vignette_gain(&soft, 75.0, 50.0, 100.0, 100.0) - 0.5f32.sqrt()).abs() < 1e-6);
    // Roundness 100 on a 200 x 100 frame: a circle of the short side (x = 150 is d = 1).
    let round = Vignette { roundness: 100.0, ..hard };
    assert!((vignette_gain(&round, 124.0, 50.0, 200.0, 100.0) - 1.0).abs() < 1e-6);
    assert!((vignette_gain(&round, 150.0, 50.0, 200.0, 100.0) - 0.5).abs() < 1e-6);
    assert!(Vignette { amount: 6.0, ..hard }.validate().is_err());
    assert!(Vignette { feather: f32::INFINITY, ..hard }.validate().is_err());
}
