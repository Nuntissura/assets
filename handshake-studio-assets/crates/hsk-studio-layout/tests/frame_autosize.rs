//! Lane SMB-ID-FRAME-AUTOSIZE-1: STU-LAY-120 first-baseline kinds and STU-LAY-119 frame auto-size on
//! the threaded reflow. Toy font of the shared fixtures: every glyph advances 6 pt, line height 12 pt,
//! ascent 10 pt (descent 2 pt). InDesign behaviour is UNVERIFIED (D5); the expectations below are
//! hand-computed from the Studio readings stated in the layout DESCRIPTOR.
mod support_layout;
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use support_layout::*;

/// Inter Regular (Type's fixture font, OFL): unitsPerEm 2816, OS/2 version 4, sCapHeight 2048,
/// sxHeight 1536. At 11 pt the scale is 11 / 2816 = 1 / 256 exactly: cap height 8 pt, x height 6 pt.
const INTER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");

/// Test-side reader (independent of the engine): `head.unitsPerEm` and OS/2 `sCapHeight` /
/// `sxHeight`, which exist only in OS/2 version 2 and later.
fn os2(bytes: &[u8]) -> (u16, Option<i16>, Option<i16>) {
    let u16_at = |o: usize| u16::from_be_bytes([bytes[o], bytes[o + 1]]);
    let u32_at = |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let tables = usize::from(u16_at(4));
    let table = |tag: &[u8; 4]| (0..tables).map(|i| 12 + 16 * i).find(|&r| &bytes[r..r + 4] == tag).map(|r| u32_at(r + 8) as usize);
    let upm = u16_at(table(b"head").expect("head") + 18);
    match table(b"OS/2") {
        Some(o) if u16_at(o) >= 2 => (upm, Some(u16_at(o + 88) as i16), Some(u16_at(o + 86) as i16)),
        _ => (upm, None, None),
    }
}
fn inter() -> Vec<u8> {
    std::fs::read(INTER).expect("Inter-Regular.ttf fixture")
}
/// The same face with its OS/2 table marked version 1 (no cap or x height).
fn inter_os2_v1() -> Vec<u8> {
    let mut b = inter();
    let n = usize::from(u16::from_be_bytes([b[4], b[5]]));
    let r = (0..n).map(|i| 12 + 16 * i).find(|&r| &b[r..r + 4] == b"OS/2").expect("OS/2");
    let o = u32::from_be_bytes([b[r + 8], b[r + 9], b[r + 10], b[r + 11]]) as usize;
    b[o..o + 2].copy_from_slice(&1u16.to_be_bytes());
    b
}
fn face_metrics(paras: Vec<ShapedParagraph>, font: &[u8]) -> Vec<ShapedParagraph> {
    let (upm, cap, x) = os2(font);
    paras.into_iter().map(|p| p.with_face_metrics(11.0, upm, cap, x)).collect()
}

fn frame(doc: &mut StudioDocument, layer: usize) -> &mut StudioTextFrame {
    doc.layers[layer].text_frame.as_mut().expect("frame")
}
/// One frame (layer 1, story 1) on page 1 with 2 pt insets.
fn single(text: &str, s: Spec) -> (StudioDocument, StudioTextStory, Vec<ShapedParagraph>) {
    let (st, paras) = story(1, text, &[(0, text.len() as u64)]);
    let mut doc = document(vec![frame_layer(1, 1, s, None, None)], [PageSide::RightHand, PageSide::LeftHand]);
    frame(&mut doc, 0).inset_spacing = edges(2.0);
    (doc, st, paras)
}
fn baselines(p: &LayoutProposal, f: usize) -> Vec<f64> {
    p.frames[f].lines.iter().map(|l| l.baseline_y_pt).collect()
}

// ---- slice 1: first baseline (STU-LAY-120) ----------------------------------------------------------

/// "aaaa bbbb cccc" sets two lines in the 60 pt inset width ("aaaa bbbb" 54 pt, "cccc"). The first
/// baseline sits at inset top 2 + max(metric, minimum 0); the second follows at the 12 pt leading.
/// Inter at 11 pt: cap 8, x 6, em box 11; toy ascent 10, leading 12. Fixed keeps the explicit offset
/// from the frame top (5 pt, unchanged behaviour).
#[test]
fn first_baseline_six_kinds_sit_at_hand_computed_offsets() {
    let font = inter();
    assert_eq!(os2(&font), (2816, Some(2048), Some(1536)));
    let (doc, st, paras) = single("aaaa bbbb cccc", spec(0, 10.0, 10.0, 64.0, 100.0));
    let paras = face_metrics(paras, &font);
    assert_eq!((paras[0].cap_height_pt, paras[0].x_height_pt, paras[0].em_box_pt), (Some(8.0), Some(6.0), Some(11.0)));
    for (kind, first) in [
        (FirstBaseline::Ascent, 12.0),
        (FirstBaseline::CapHeight, 10.0),
        (FirstBaseline::Leading, 14.0),
        (FirstBaseline::EmBoxHeight, 13.0),
        (FirstBaseline::XHeight, 8.0),
        (FirstBaseline::Fixed, 5.0),
    ] {
        let mut d = doc.clone();
        frame(&mut d, 0).first_baseline_offset = kind;
        frame(&mut d, 0).minimum_first_baseline_offset = len(if kind == FirstBaseline::Fixed { 5.0 } else { 0.0 });
        let p = run(&d, &st, &paras, &records()).unwrap_or_else(|e| panic!("{kind:?}: {e}"));
        assert_eq!(baselines(&p, 0), [first, first + 12.0], "{kind:?}");
        assert_eq!(p.overset, None);
    }
}

/// The minimum first-baseline offset wins when it is larger than the metric (ascent 10 < 20: 2 + 20),
/// the metric when it is larger (x height 6 > 5: 2 + 6). In a thread each frame takes its own kind:
/// frame A (fixed 12 pt, 16 pt high, 2 pt insets) holds one line; frame B (cap height) sets "cccc" at
/// its inset top 2 + cap 8 = 10.
#[test]
fn minimum_offset_wins_when_larger_and_each_thread_frame_takes_its_own_kind() {
    let font = inter();
    let (doc, st, paras) = single("aaaa bbbb cccc", spec(0, 10.0, 10.0, 64.0, 100.0));
    let paras = face_metrics(paras, &font);
    for (kind, min, first) in [(FirstBaseline::Ascent, 20.0, 22.0), (FirstBaseline::XHeight, 5.0, 8.0)] {
        let mut d = doc.clone();
        frame(&mut d, 0).first_baseline_offset = kind;
        frame(&mut d, 0).minimum_first_baseline_offset = len(min);
        assert_eq!(baselines(&run(&d, &st, &paras, &records()).expect("placed"), 0), [first, first + 12.0], "{kind:?}");
    }
    let text = "aaaa bbbb cccc";
    let (st, paras) = story(1, text, &[(0, text.len() as u64)]);
    let paras = face_metrics(paras, &font);
    let mut d = document(
        vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 64.0, 16.0), None, Some(2)), frame_layer(2, 1, spec(0, 10.0, 100.0, 64.0, 40.0), Some(1), None)],
        [PageSide::RightHand, PageSide::LeftHand],
    );
    for l in 0..2 {
        frame(&mut d, l).inset_spacing = edges(2.0);
    }
    frame(&mut d, 0).minimum_first_baseline_offset = len(12.0);
    frame(&mut d, 1).first_baseline_offset = FirstBaseline::CapHeight;
    frame(&mut d, 1).minimum_first_baseline_offset = len(0.0);
    let p = run(&d, &st, &paras, &records()).expect("placed");
    assert_eq!((baselines(&p, 0), baselines(&p, 1)), (vec![12.0], vec![10.0]));
    assert_eq!(p.overset, None);
}

/// A face without OS/2 cap/x height (OS/2 version 1) and a paragraph without face metrics (no em box)
/// are refused for the kinds that need them; the typed reason is `first_baseline_metric_unavailable:
/// <kind>` and the reflow refusal stays `unsupported_composition`. Ascent and leading need no face.
#[test]
fn missing_face_metric_refuses_typed() {
    let v1 = inter_os2_v1();
    assert_eq!(os2(&v1), (2816, None, None));
    let (doc, st, plain) = single("aaaa bbbb cccc", spec(0, 10.0, 10.0, 64.0, 100.0));
    let paras = face_metrics(plain.clone(), &v1);
    for (kind, paras, code) in [
        (FirstBaseline::CapHeight, &paras, "first_baseline_metric_unavailable:cap_height"),
        (FirstBaseline::XHeight, &paras, "first_baseline_metric_unavailable:x_height"),
        (FirstBaseline::EmBoxHeight, &plain, "first_baseline_metric_unavailable:em_box_height"),
    ] {
        assert_eq!(paras[0].first_baseline_metric(kind).unwrap_err().code(), code);
        let mut d = doc.clone();
        frame(&mut d, 0).first_baseline_offset = kind;
        frame(&mut d, 0).minimum_first_baseline_offset = len(0.0);
        assert_eq!(run(&d, &st, paras, &records()), Err(LayoutError::UnsupportedComposition), "{kind:?}");
    }
    assert_eq!(paras[0].em_box_pt, Some(11.0));
    for kind in [FirstBaseline::Ascent, FirstBaseline::Leading] {
        let mut d = doc.clone();
        frame(&mut d, 0).first_baseline_offset = kind;
        frame(&mut d, 0).minimum_first_baseline_offset = len(0.0);
        assert!(run(&d, &st, &plain, &records()).is_ok(), "{kind:?}");
    }
}

/// Hostile metrics (V4-01/02): a NaN, infinite or negative first-baseline metric, and a negative
/// minimum offset, are `invalid_geometry`, never a baseline.
#[test]
fn hostile_first_baseline_metrics_are_invalid_geometry() {
    let (doc, st, paras) = single("aaaa bbbb cccc", spec(0, 10.0, 10.0, 64.0, 100.0));
    let mut d = doc.clone();
    frame(&mut d, 0).first_baseline_offset = FirstBaseline::CapHeight;
    frame(&mut d, 0).minimum_first_baseline_offset = len(0.0);
    for bad in [f64::NAN, f64::INFINITY, -1.0] {
        let mut p = paras.clone();
        p[0].cap_height_pt = Some(bad);
        assert_eq!(run(&d, &st, &p, &records()), Err(LayoutError::InvalidGeometry), "{bad}");
    }
    let mut p = paras.clone();
    p[0].cap_height_pt = Some(7.0);
    frame(&mut d, 0).minimum_first_baseline_offset = len(-1.0);
    assert_eq!(run(&d, &st, &p, &records()), Err(LayoutError::InvalidGeometry));
    frame(&mut d, 0).minimum_first_baseline_offset = len(0.0);
    assert_eq!(baselines(&run(&d, &st, &p, &records()).expect("placed"), 0), [9.0, 21.0]);
}
