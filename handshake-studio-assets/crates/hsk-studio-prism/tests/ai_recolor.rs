//! Lane SMB-AI-TYPE-PATH-RECOLOR slice 2: deterministic harmony, recolour table, sliders and colour
//! reduction (STU-COL-235, STU-COL-236, STU-VEC-142). Expected values are hand-derived from HSB
//! and from the sRGB transfer (0.5 encoded -> ((0.5 + 0.055) / 1.055)^2.4 = 0.214041140 linear,
//! the luminance of grey 0.5). Illustrator Recolor Artwork parity is UNVERIFIED (D5).
use hsk_studio_prism::*;

const RED: Rgb = [1.0, 0.0, 0.0];
const GREEN: Rgb = [0.0, 1.0, 0.0];
const BLUE: Rgb = [0.0, 0.0, 1.0];
const WHITE: Rgb = [1.0, 1.0, 1.0];
const BLACK: Rgb = [0.0, 0.0, 0.0];
const GREY: Rgb = [0.5, 0.5, 0.5];
const Y_GREY: f64 = 0.214_041_140_482_232_54;

fn near(a: Rgb, b: Rgb) {
    assert!(a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9), "{a:?} != {b:?}");
}
fn w(rgb: Rgb, weight: f64) -> WeightedColour {
    WeightedColour { rgb, weight }
}

#[test]
fn ai_recolor_harmony_rules_are_pure_and_snap_to_a_library() {
    let h = |rule, n, step| harmony(RED, rule, n, step, None).unwrap();
    assert_eq!(h(HarmonyRule::Complementary, 2, 0.0), vec![RED, [0.0, 1.0, 1.0]]);
    assert_eq!(h(HarmonyRule::Triad, 3, 0.0), vec![RED, GREEN, BLUE]);
    // A fourth triad member starts the next cycle one step (50 %) darker.
    assert_eq!(h(HarmonyRule::Triad, 4, 50.0)[3], [0.5, 0.0, 0.0]);
    assert_eq!(h(HarmonyRule::Shades, 3, 25.0), vec![RED, [0.75, 0.0, 0.0], [0.5, 0.0, 0.0]]);
    // Tints: saturation halves, brightness stays 1 -> (1, 0.5, 0.5).
    assert_eq!(h(HarmonyRule::Tints, 2, 50.0)[1], [1.0, 0.5, 0.5]);
    // Split complementary: hues 150 and 210.
    let s = h(HarmonyRule::SplitComplementary, 3, 0.0);
    near(s[1], [0.0, 1.0, 0.5]);
    near(s[2], [0.0, 0.5, 1.0]);
    // Pure: the same inputs give the same palette.
    assert_eq!(h(HarmonyRule::Compound, 8, 10.0), h(HarmonyRule::Compound, 8, 10.0));
    // limit_to_library: every member is the nearest library colour.
    let library = [[0.9, 0.0, 0.0], [0.0, 0.8, 0.8]];
    assert_eq!(harmony(RED, HarmonyRule::Complementary, 2, 0.0, Some(&library)).unwrap(), vec![[0.9, 0.0, 0.0], [0.0, 0.8, 0.8]]);
    // Hostile input.
    assert_eq!(harmony(RED, HarmonyRule::Triad, 0, 0.0, None), Err(RecolorError::InvalidInput("count")));
    assert_eq!(harmony(RED, HarmonyRule::Triad, MAX_HARMONY_COLOURS + 1, 0.0, None), Err(RecolorError::OverLimit("count")));
    assert_eq!(harmony([f64::NAN, 0.0, 0.0], HarmonyRule::Triad, 1, 0.0, None), Err(RecolorError::InvalidInput("base")));
    assert_eq!(harmony(RED, HarmonyRule::Triad, 1, 101.0, None), Err(RecolorError::InvalidInput("step")));
    assert_eq!(harmony(RED, HarmonyRule::Triad, 1, 0.0, Some(&[])), Err(RecolorError::InvalidInput("library")));
}

#[test]
fn ai_recolor_table_methods_preserve_toggles_and_sliders() {
    let table = |method, current: Vec<Rgb>, new| RecolorTable { rows: vec![RecolorRow { current, new }], method, ..Default::default() };
    let off = RecolorAdjust::default();
    // Exact: every member becomes the new colour.
    let t = table(ColorizeMethod::Exact, vec![BLACK, GREY], Some(RED));
    assert_eq!((recolor(&t, &off, BLACK), recolor(&t, &off, GREY)), (RED, RED));
    // Preserve tints (key black, Y = 0): grey keeps tint t = 1 - Y(grey) of the new colour,
    // tint 1 - t (1 - c) per channel = (1, Y, Y); white (t = 0) stays paper white.
    let t = table(ColorizeMethod::PreserveTints, vec![BLACK, GREY, WHITE], Some(RED));
    near(recolor(&t, &off, GREY), [1.0, Y_GREY, Y_GREY]);
    near(recolor(&t, &off, WHITE), WHITE);
    near(recolor(&t, &off, BLACK), RED);
    // Scale tints: the DARKEST member (black, listed second) is the reference.
    let t = table(ColorizeMethod::ScaleTints, vec![GREY, BLACK], Some(RED));
    near(recolor(&t, &off, BLACK), RED);
    near(recolor(&t, &off, GREY), [1.0, Y_GREY, Y_GREY]);
    // Tints and shades around a grey key: black (darker) -> shade 0 = black, white -> tint 0.
    let t = table(ColorizeMethod::TintsAndShades, vec![GREY, BLACK, WHITE], Some(BLUE));
    near(recolor(&t, &off, GREY), BLUE);
    near(recolor(&t, &off, BLACK), BLACK);
    near(recolor(&t, &off, WHITE), WHITE);
    // Hue shift: key red (0 deg) -> green (120 deg); (0.5, 0.25, 0.25) = HSB (0, 0.5, 0.5) -> (120, 0.5, 0.5).
    let t = table(ColorizeMethod::HueShift, vec![RED, [0.5, 0.25, 0.25]], Some(GREEN));
    near(recolor(&t, &off, [0.5, 0.25, 0.25]), [0.25, 0.5, 0.25]);
    // Excluded rows, colours in no row and preserved colours are unchanged.
    let t = table(ColorizeMethod::Exact, vec![GREY], None);
    assert_eq!(recolor(&t, &off, GREY), GREY);
    assert_eq!(recolor(&t, &off, [0.1, 0.2, 0.3]), [0.1, 0.2, 0.3]);
    let t = RecolorTable { preserve_white: true, preserve_grays: true, ..table(ColorizeMethod::Exact, vec![WHITE, GREY, BLACK], Some(RED)) };
    assert_eq!((recolor(&t, &off, WHITE), recolor(&t, &off, GREY), recolor(&t, &off, BLACK)), (WHITE, GREY, RED));
    // STU-VEC-142 sliders and the wheel rotation.
    let none = RecolorTable::default();
    let a = |adjust: RecolorAdjust, c| recolor(&none, &adjust, c);
    near(a(RecolorAdjust { saturation_shift: -100.0, ..off }, RED), WHITE);
    near(a(RecolorAdjust { brightness_shift: -50.0, ..off }, RED), [0.5, 0.0, 0.0]);
    near(a(RecolorAdjust { hue_shift_deg: 120.0, ..off }, RED), GREEN);
    near(a(RecolorAdjust { temperature_shift: 100.0, ..off }, GREY), [0.6, 0.5, 0.4]);
    near(a(RecolorAdjust { luminosity_shift: 50.0, ..off }, GREY), [0.75, 0.75, 0.75]);
    // Hostile tables and sliders.
    assert_eq!(table(ColorizeMethod::Exact, vec![], Some(RED)).validate(), Err(RecolorError::InvalidInput("empty_row")));
    assert_eq!(table(ColorizeMethod::Exact, vec![[2.0, 0.0, 0.0]], Some(RED)).validate(), Err(RecolorError::InvalidInput("row_colour")));
    assert_eq!(table(ColorizeMethod::Exact, vec![RED], Some([0.0, f64::NAN, 0.0])).validate(), Err(RecolorError::InvalidInput("new_colour")));
    assert_eq!(RecolorAdjust { hue_shift_deg: f64::INFINITY, ..off }.validate(), Err(RecolorError::InvalidInput("adjust")));
}

#[test]
fn ai_recolor_extraction_and_reduction() {
    // Equal triples merge, prominence (summed weight) orders the rows.
    let found = extract_colours(&[w(BLUE, 5.0), w(RED, 4.0), w(RED, 6.0), w(WHITE, 3.0)]).unwrap();
    assert_eq!(found.iter().map(|c| (c.rgb, c.weight)).collect::<Vec<_>>(), vec![(RED, 10.0), (BLUE, 5.0), (WHITE, 3.0)]);
    // Reduce to 2 with white preserved: red and dark red (closest) merge, keyed by the heavier red.
    let colours = [w(RED, 10.0), w([0.9, 0.0, 0.0], 1.0), w(BLUE, 5.0), w(WHITE, 3.0)];
    let rows = reduce_colours(&colours, 2, true, false, false).unwrap();
    assert_eq!(rows, vec![RecolorRow { current: vec![RED, [0.9, 0.0, 0.0]], new: Some(RED) }, RecolorRow { current: vec![BLUE], new: Some(BLUE) }]);
    // Reduce to a library: each colour joins its nearest member.
    let rows = reduce_to_library(&colours, &[RED, BLUE], true, false, false).unwrap();
    assert_eq!(rows, vec![RecolorRow { current: vec![RED, [0.9, 0.0, 0.0]], new: Some(RED) }, RecolorRow { current: vec![BLUE], new: Some(BLUE) }]);
    // Hostile sizes and values, refused before the work.
    let many: Vec<WeightedColour> = (0..=MAX_REDUCE_COLOURS).map(|i| w([i as f64 / 1024.0, 0.0, 0.0], 1.0)).collect();
    assert_eq!(reduce_colours(&many, 2, false, false, false), Err(RecolorError::OverLimit("reduce_colours")));
    let huge = vec![w(RED, 1.0); MAX_RECOLOR_COLOURS + 1];
    assert_eq!(extract_colours(&huge), Err(RecolorError::OverLimit("colours")));
    assert_eq!(extract_colours(&[w(RED, f64::NAN)]), Err(RecolorError::InvalidInput("weight")));
    assert_eq!(reduce_colours(&colours, 0, false, false, false), Err(RecolorError::InvalidInput("target_count")));
}
