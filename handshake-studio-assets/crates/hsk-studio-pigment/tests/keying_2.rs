//! The remaining keyers of the STU-CMP-032 table (lane SMB-AE-KEY-TRACK-2): colour key,
//! colour-difference key, linear colour key, extract, inner/outer key, key cleaner, wire removal and
//! unmult. Synthetic planes; expected values derived by hand from the rules in `src/keying.rs`.
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::keying::*;
use hsk_studio_pigment::retouch::RgbaPlane;

fn row(pixels: &[[f32; 4]]) -> RgbaPlane {
    RgbaPlane::new(Rect { x: 0, y: 0, width: pixels.len() as u32, height: 1 }, pixels.to_vec()).unwrap()
}
fn plane(w: u32, h: u32, f: impl Fn(u32, u32) -> [f32; 4]) -> RgbaPlane {
    let mut data = Vec::new();
    for y in 0..h {
        for x in 0..w {
            data.push(f(x, y));
        }
    }
    RgbaPlane::new(Rect { x: 0, y: 0, width: w, height: h }, data).unwrap()
}
fn run(src: &RgbaPlane, stages: &[KeyStage]) -> KeyResult {
    run_key_pipeline(src, stages, None, &KeyLimits::DEFAULT).unwrap()
}
fn close(a: f32, b: f64) -> bool {
    (f64::from(a) - b).abs() < 1e-6
}
const R: MatteCombine = MatteCombine::Replace;

#[test]
fn colour_difference_linear_extract_and_unmult_on_known_colours() {
    // Colour key: RGB distance / sqrt 3 against the tolerance.
    let src = row(&[[0.0, 0.95, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0]]);
    let m = run(&src, &[KeyStage::ColourKey(ColourKey { key: [0.0, 1.0, 0.0], tolerance: 0.1 }, R)]).matte.data;
    assert_eq!(m, vec![0.0, 1.0]);
    // Colour-difference key, green screen: screen, subject, spill.
    let cd = KeyStage::ColourDifference(ColourDifferenceKey { screen: SpillScreen::Green, a_black: 0.1, a_white: 0.5, b_black: 0.2, b_white: 0.6 }, R);
    let src = row(&[[0.1, 0.9, 0.1, 1.0], [0.8, 0.5, 0.4, 1.0], [0.4, 0.6, 0.3, 1.0]]);
    let m = run(&src, &[cd]).matte.data;
    assert!(close(m[0], 0.0) && close(m[1], 1.0), "{m:?}");
    // Spill: A = 1 - (0.2 - 0.1) / 0.4 = 0.75, B = 1 - (0.25 - 0.2) / 0.4 = 0.875; max = 0.875.
    assert!(close(m[2], 0.875), "{m:?}");
    // Linear colour key on hue: same hue removed, opposite hue and grey kept; `keep` inverts.
    let src = row(&[[0.2, 0.6, 0.2, 1.0], [0.0, 0.0, 1.0, 1.0], [0.5, 0.5, 0.5, 1.0]]);
    let lin = |keep| KeyStage::LinearColour(LinearColourKey { key: [0.0, 1.0, 0.0], matching: LinearMatch::Hue, tolerance: 0.05, softness: 0.1, keep }, R);
    assert_eq!(run(&src, &[lin(false)]).matte.data, vec![0.0, 1.0, 1.0]);
    assert_eq!(run(&src, &[lin(true)]).matte.data, vec![1.0, 0.0, 0.0]);
    // Hue 60 degrees away: d = 60/180, ramp (1/3 - 0.05) / 0.1 > 1 -> kept fully.
    let yellow = row(&[[1.0, 1.0, 0.0, 1.0]]);
    assert_eq!(run(&yellow, &[lin(false)]).matte.data, vec![1.0]);
    // Rgb matching inside the softness ramp: d = 0.1 / sqrt 3 = 0.0577..., (d - 0.05) / 0.1.
    let near = row(&[[0.1, 1.0, 0.0, 1.0]]);
    let rgb = KeyStage::LinearColour(LinearColourKey { key: [0.0, 1.0, 0.0], matching: LinearMatch::Rgb, tolerance: 0.05, softness: 0.1, keep: false }, R);
    let want = (0.1f64 / 3f64.sqrt() - 0.05f64) / 0.1;
    let got = run(&near, &[rgb]).matte.data[0];
    assert!((f64::from(got) - want).abs() < 1e-6, "{got} vs {want}");
    // Extract on luminance with soft ends.
    let greys = row(&[[0.5; 4], [0.25, 0.25, 0.25, 1.0], [0.1, 0.1, 0.1, 1.0], [0.75, 0.75, 0.75, 1.0]]);
    let ex = |invert| KeyStage::Extract(Extract { channel: ExtractChannel::Luminance, black: 0.3, white: 0.7, black_softness: 0.1, white_softness: 0.1, invert }, R);
    let m = run(&greys, &[ex(false)]).matte.data;
    assert!(close(m[0], 1.0) && close(m[1], 0.5) && close(m[2], 0.0) && close(m[3], 0.5), "{m:?}");
    let mi = run(&greys, &[ex(true)]).matte.data;
    assert!(m.iter().zip(&mi).all(|(a, b)| close(*a + *b, 1.0)));
    // Unmult: colour / max, alpha * max; black becomes transparent.
    let out = run(&row(&[[0.5, 0.25, 0.0, 1.0], [0.0, 0.0, 0.0, 1.0]]), &[KeyStage::Unmult]).colour.data;
    assert_eq!(out[0], [1.0, 0.5, 0.0, 0.5]);
    assert_eq!(out[1][3], 0.0);
    assert_eq!(KeyStage::Unmult.key(), "key_unmult");
}

#[test]
fn inner_outer_key_cleaner_and_wire_removal() {
    // White object (columns 0..9), two blend columns, blue background (11..20).
    let src = plane(20, 6, |x, _| match x {
        0..=8 => [1.0, 1.0, 1.0, 1.0],
        9 => [0.75, 0.75, 1.0, 1.0],
        10 => [0.25, 0.25, 1.0, 1.0],
        _ => [0.0, 0.0, 1.0, 1.0],
    });
    let rect = |x1: f64| vec![[-1.0, -1.0], [x1, -1.0], [x1, 7.0], [-1.0, 7.0]];
    let io = KeyStage::InnerOuter(InnerOuterKey { inner: rect(9.0), outer: rect(11.0) }, R);
    let m = run(&src, &[io]).matte.data;
    for y in 0..6 {
        let r = &m[y * 20..y * 20 + 20];
        assert!(r[..9].iter().all(|v| *v == 1.0) && r[11..].iter().all(|v| *v == 0.0), "{r:?}");
        assert!(close(r[9], 0.75) && close(r[10], 0.25), "band alpha by projection: {r:?}");
    }
    // Key cleaner: a feathered hard edge; contrast 2 pushes partial values apart, far pixels keep theirs.
    let step = plane(20, 6, |x, _| if x < 10 { [0.0, 0.0, 0.0, 1.0] } else { [1.0; 4] });
    let key = KeyStage::Luma(LumaKey { kind: LumaKeyType::KeyOutDarker, threshold: 0.5, tolerance: 0.0, softness: 0.0 }, R);
    let soft = run(&step, &[key.clone(), KeyStage::Feather { radius_px: 2.0 }]).matte.data;
    let clean = run(&step, &[key, KeyStage::Feather { radius_px: 2.0 }, KeyStage::KeyCleaner(KeyCleaner { edge_radius_px: 2, alpha_contrast: 2.0 })]).matte.data;
    let at = |v: &Vec<f32>, x: usize| v[2 * 20 + x];
    assert!(at(&soft, 9) > 0.0 && at(&soft, 9) < 0.5 && at(&soft, 10) > 0.5 && at(&soft, 10) < 1.0);
    assert!(at(&clean, 9) < at(&soft, 9) && at(&clean, 10) > at(&soft, 10), "{} {} -> {} {}", at(&soft, 9), at(&soft, 10), at(&clean, 9), at(&clean, 10));
    assert_eq!(at(&clean, 0), at(&soft, 0));
    assert_eq!(at(&clean, 19), at(&soft, 19));
    // Wire removal: a dark wire on row 10 of a grey plate disappears (hard band).
    let wired = plane(20, 20, |_, y| if y == 10 { [0.0, 0.0, 0.0, 1.0] } else { [0.5, 0.5, 0.5, 1.0] });
    let wire = |slope| KeyStage::WireRemoval(WireRemoval { from: [0.0, 10.5], to: [20.0, 10.5], thickness_px: 2.0, slope });
    let out = run(&wired, &[wire(0.0)]).colour.data;
    assert!(out.iter().all(|p| *p == [0.5, 0.5, 0.5, 1.0]), "{:?}", &out[200..220]);
    // A soft band (thickness 4, slope 1): weight 1 on the wire, 0.5 one pixel off it.
    let soft_wire = KeyStage::WireRemoval(WireRemoval { from: [0.0, 10.5], to: [20.0, 10.5], thickness_px: 4.0, slope: 1.0 });
    let lined = plane(20, 20, |_, y| if (9..=11).contains(&y) { [0.0, 0.0, 0.0, 1.0] } else { [0.5, 0.5, 0.5, 1.0] });
    let out = run(&lined, &[soft_wire]).colour.data;
    assert!(close(out[10 * 20 + 5][0], 0.5) && close(out[9 * 20 + 5][0], 0.25) && close(out[11 * 20 + 5][0], 0.25), "{:?} {:?}", out[9 * 20 + 5], out[10 * 20 + 5]);
}

#[test]
fn new_keyers_refuse_hostile_parameters_before_allocation() {
    let src = plane(8, 8, |_, _| [0.5; 4]);
    let refused = |s: KeyStage| assert_eq!(run_key_pipeline(&src, std::slice::from_ref(&s), None, &KeyLimits::DEFAULT).unwrap_err(), KeyError::Parameter, "{s:?}");
    refused(KeyStage::ColourKey(ColourKey { key: [0.0, f32::NAN, 0.0], tolerance: 0.1 }, R));
    refused(KeyStage::ColourDifference(ColourDifferenceKey { screen: SpillScreen::Blue, a_black: 0.5, a_white: 0.5, b_black: 0.0, b_white: 1.0 }, R));
    refused(KeyStage::LinearColour(LinearColourKey { key: [0.0; 3], matching: LinearMatch::Chroma, tolerance: 2.0, softness: 0.1, keep: false }, R));
    refused(KeyStage::Extract(Extract { channel: ExtractChannel::Alpha, black: 0.8, white: 0.2, black_softness: 0.0, white_softness: 0.0, invert: false }, R));
    refused(KeyStage::InnerOuter(InnerOuterKey { inner: vec![[0.0, 0.0], [1.0, 1.0]], outer: vec![[0.0, 0.0], [9.0, 0.0], [9.0, 9.0]] }, R));
    refused(KeyStage::InnerOuter(InnerOuterKey { inner: vec![[0.0, 0.0]; MAX_KEY_PATH_VERTICES + 1], outer: vec![[0.0, 0.0], [9.0, 0.0], [9.0, 9.0]] }, R));
    refused(KeyStage::InnerOuter(InnerOuterKey { inner: vec![[0.0, 0.0], [f64::INFINITY, 0.0], [9.0, 9.0]], outer: vec![[0.0, 0.0], [9.0, 0.0], [9.0, 9.0]] }, R));
    refused(KeyStage::KeyCleaner(KeyCleaner { edge_radius_px: 0, alpha_contrast: 1.0 }));
    refused(KeyStage::KeyCleaner(KeyCleaner { edge_radius_px: 2, alpha_contrast: 5.0 }));
    refused(KeyStage::WireRemoval(WireRemoval { from: [0.0, 0.0], to: [1.0, 1.0], thickness_px: 100.0, slope: 0.0 }));
    refused(KeyStage::WireRemoval(WireRemoval { from: [f64::NAN, 0.0], to: [1.0, 1.0], thickness_px: 2.0, slope: 0.0 }));
    // Work is admitted before allocation: the cleaner's window over a big plane under a tight budget.
    let tight = KeyLimits { max_bytes: u64::MAX, max_work: 1_000 };
    let e = run_key_pipeline(&src, &[KeyStage::KeyCleaner(KeyCleaner { edge_radius_px: 64, alpha_contrast: 1.0 })], None, &tight).unwrap_err();
    assert!(matches!(e, KeyError::OverBudget { limit: 1_000, .. }), "{e:?}");
    // Every new kind has its own operator key (the envelope's capability unit).
    let keys = [
        KeyKind::ColourKey, KeyKind::ColourDifference, KeyKind::LinearColour, KeyKind::Extract,
        KeyKind::InnerOuter, KeyKind::KeyCleaner, KeyKind::WireRemoval, KeyKind::Unmult,
    ]
    .map(KeyKind::key);
    let mut sorted = keys.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 8, "{keys:?}");
}
