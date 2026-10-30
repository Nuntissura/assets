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

// ---- slice 2: auto-size (STU-LAY-119) ---------------------------------------------------------------

fn autosize_with(
    doc: &StudioDocument,
    st: &StudioTextStory,
    paras: &[ShapedParagraph],
    cancel: &hsk_studio_accord::CancellationToken,
) -> Result<AutosizeProposal, LayoutError> {
    reflow_autosize(
        &ReflowRequest {
            document: doc,
            story: st,
            resolved_text: None,
            paragraphs: paras,
            obstacles: &[],
            composer: Composer::FirstFit,
            space: SpaceParams::default(),
            expected_revision: doc.revision,
            limits: Limits::default(),
        },
        &TypesetOptions::default(),
        &records(),
        cancel,
    )
}
fn autosize(doc: &StudioDocument, st: &StudioTextStory, paras: &[ShapedParagraph]) -> Result<AutosizeProposal, LayoutError> {
    autosize_with(doc, st, paras, &Default::default())
}
/// The one resize: (before w, before h, after w, after h).
fn resized(p: &AutosizeProposal) -> (f64, f64, f64, f64) {
    assert_eq!(p.autosized_frames().len(), 1);
    let r = &p.autosized_frames()[0];
    (r.before_width_pt, r.before_height_pt, r.after_width_pt, r.after_height_pt)
}
fn sized(text: &str, s: Spec, kind: AutoSizing) -> (StudioDocument, StudioTextStory, Vec<ShapedParagraph>) {
    let (mut doc, st, paras) = single(text, s);
    frame(&mut doc, 0).auto_sizing_type = kind;
    (doc, st, paras)
}

/// Height only: the frame keeps its top edge and width; its height is the last line's bottom plus the
/// bottom inset. 60 pt inset width sets two 4-letter words per line; fixed first baseline 10, leading
/// 12, descent 2, insets 2. Three lines: last baseline 34, height 34 + 2 + 2 = 38 (the authored 100
/// shrinks). One more line: 46 + 4 = 50 (an authored 20 grows). One line: 10 + 4 = 14. Two columns
/// (60 pt each, gutter 4): three lines fit at the smallest height holding two lines in column 0 and
/// one in column 1: 22 + 4 = 26. The lines are those of the sized frame (`reflow_flow` agrees).
#[test]
fn height_only_fits_the_last_line_and_grows_and_shrinks() {
    for (text, h, after) in [
        ("aaaa bbbb cccc dddd eeee ffff", 100.0, 38.0),
        ("aaaa bbbb cccc dddd eeee ffff gggg", 20.0, 50.0),
        ("aaaa", 100.0, 14.0),
    ] {
        let (doc, st, paras) = sized(text, spec(0, 10.0, 10.0, 64.0, h), AutoSizing::HeightOnly);
        let p = autosize(&doc, &st, &paras).expect("auto-sized");
        assert_eq!(resized(&p), (64.0, h, 64.0, after), "{text}");
        assert_eq!(p.layout.overset, None);
        let last = baselines(&p.layout, 0).last().copied().expect("lines");
        assert_eq!(last + 2.0 + 2.0, after);
        assert_eq!(run(&doc, &st, &paras, &records()).expect("reflow"), p.layout);
    }
    let mut s = spec(0, 10.0, 10.0, 128.0, 100.0);
    s.columns = 2;
    s.gutter = 4.0;
    let (doc, st, paras) = sized("aaaa bbbb cccc dddd eeee ffff", s, AutoSizing::HeightOnly);
    let p = autosize(&doc, &st, &paras).expect("auto-sized");
    assert_eq!(resized(&p), (128.0, 100.0, 128.0, 26.0));
    let placed: Vec<(usize, f64)> = p.layout.frames[0].lines.iter().map(|l| (l.column, l.baseline_y_pt)).collect();
    assert_eq!(placed, [(0, 10.0), (0, 22.0), (1, 10.0)]);
    assert!(p.autosized_frames()[0].compositions <= MAX_AUTOSIZE_ITERATIONS);
    // Vertical justification is moot for an auto height: centred, the shorter column 1 keeps its
    // line at the top.
    let mut centred = doc.clone();
    frame(&mut centred, 0).vertical_justification = VerticalJustification::Center;
    assert_eq!(autosize(&centred, &st, &paras).expect("centred").layout, p.layout);
}

/// Width only: lines do not wrap; the width is the widest line plus the insets (left 2, right 3),
/// the height stays. "aaaa bbbb cccc" is 84 pt, "dd" 12 pt: width 84 + 5 = 89. Height and width: the
/// same width and the height of the two lines: 22 + 2 + 2 = 26.
#[test]
fn width_only_is_the_widest_line_plus_the_insets() {
    let text = "aaaa bbbb cccc\ndd";
    for (kind, after_h) in [(AutoSizing::WidthOnly, 100.0), (AutoSizing::HeightAndWidth, 26.0)] {
        let (st, paras) = story(1, text, &[(0, 14), (15, 17)]);
        let mut doc = document(vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 64.0, 100.0), None, None)], [PageSide::RightHand, PageSide::LeftHand]);
        frame(&mut doc, 0).inset_spacing = LayoutEdges { top: len(2.0), right: len(3.0), bottom: len(2.0), left: len(2.0) };
        frame(&mut doc, 0).auto_sizing_type = kind;
        let p = autosize(&doc, &st, &paras).expect("auto-sized");
        assert_eq!(resized(&p), (64.0, 100.0, 89.0, after_h), "{kind:?}");
        let lines: Vec<(f64, f64, f64)> = p.layout.frames[0].lines.iter().map(|l| (l.baseline_y_pt, l.x_start_pt, l.width_pt)).collect();
        assert_eq!(lines, [(10.0, 2.0, 84.0), (22.0, 2.0, 12.0)], "{kind:?}");
        assert_eq!(p.layout.overset, None);
        if kind == AutoSizing::WidthOnly {
            // Width only keeps the authored height and its vertical justification: bottom moves the
            // block (line tops 0 to bottoms 24) onto the bottom inset 98, 74 pt down.
            frame(&mut doc, 0).vertical_justification = VerticalJustification::Bottom;
            assert_eq!(baselines(&autosize(&doc, &st, &paras).expect("bottom").layout, 0), [84.0, 96.0]);
        }
    }
}

/// Proportional: the smallest uniform scale s of the authored 64 x 32 frame (insets 2, fixed first
/// baseline 10) at which the story fits. Three lines of two words need 32 s >= 34 + 2 + 2 (s >=
/// 1.1875) and fit the inset width 64 s - 4 >= 54; below 1.1875 two words per line still need three
/// lines and one word per line needs six: s = 1.1875, 76 x 38. Bisection stops within
/// AUTOSIZE_TOLERANCE_PT on the larger side; the aspect ratio is kept.
#[test]
fn proportional_keeps_the_aspect_and_fits() {
    let (doc, st, paras) = sized("aaaa bbbb cccc dddd eeee ffff", spec(0, 10.0, 10.0, 64.0, 32.0), AutoSizing::HeightAndWidthProportionally);
    let p = autosize(&doc, &st, &paras).expect("auto-sized");
    let (bw, bh, w, h) = resized(&p);
    assert_eq!((bw, bh), (64.0, 32.0));
    assert!((76.0..=76.0 + AUTOSIZE_TOLERANCE_PT).contains(&w), "{w}");
    assert!((w / h - 2.0).abs() < 1e-12, "{w} x {h}");
    assert_eq!(p.layout.overset, None);
    assert_eq!(baselines(&p.layout, 0), [10.0, 22.0, 34.0]);
    let r = &p.autosized_frames()[0];
    assert!(r.compositions >= 2 && r.compositions <= MAX_AUTOSIZE_ITERATIONS, "{}", r.compositions);
    // Just below the found scale the story no longer fits the frame.
    let mut below = doc.clone();
    frame(&mut below, 0).auto_sizing_type = AutoSizing::Off;
    frame(&mut below, 0).bounds.width = len(75.9);
    frame(&mut below, 0).bounds.height = len(37.95);
    assert!(run(&below, &st, &paras, &records()).expect("reflow").overset.is_some());
}

/// Auto-size only on the last frame of a thread (or an unthreaded frame): a frame with a next frame
/// is refused typed. Width and proportional kinds with more than one column are refused typed. Off is
/// the plain reflow with no resize.
#[test]
fn mid_thread_and_multi_column_autosize_refuse_typed_and_off_is_unchanged() {
    let text = "aaaa bbbb cccc";
    let (st, paras) = story(1, text, &[(0, text.len() as u64)]);
    let thread = document(
        vec![frame_layer(1, 1, spec(0, 10.0, 10.0, 64.0, 16.0), None, Some(2)), frame_layer(2, 1, spec(0, 10.0, 100.0, 64.0, 40.0), Some(1), None)],
        [PageSide::RightHand, PageSide::LeftHand],
    );
    for kind in [AutoSizing::HeightOnly, AutoSizing::WidthOnly, AutoSizing::HeightAndWidth, AutoSizing::HeightAndWidthProportionally] {
        let mut d = thread.clone();
        frame(&mut d, 0).auto_sizing_type = kind;
        assert_eq!(autosize(&d, &st, &paras), Err(LayoutError::AutosizeThreadedFrame), "{kind:?}");
        assert_eq!(run(&d, &st, &paras, &records()).unwrap_err().code(), "autosize_threaded_frame");
        // The last frame of the thread auto-sizes.
        let mut d = thread.clone();
        frame(&mut d, 1).auto_sizing_type = kind;
        assert_eq!(autosize(&d, &st, &paras).expect("last frame").autosized_frames().len(), 1, "{kind:?}");
    }
    let mut s = spec(0, 10.0, 10.0, 128.0, 100.0);
    s.columns = 2;
    for kind in [AutoSizing::WidthOnly, AutoSizing::HeightAndWidth, AutoSizing::HeightAndWidthProportionally] {
        let (doc, st, paras) = sized(text, s, kind);
        assert_eq!(autosize(&doc, &st, &paras), Err(LayoutError::AutosizeMultiColumn), "{kind:?}");
    }
    let (doc, st, paras) = sized(text, spec(0, 10.0, 10.0, 64.0, 100.0), AutoSizing::Off);
    let p = autosize(&doc, &st, &paras).expect("off");
    assert!(p.autosized_frames().is_empty());
    assert_eq!(p.layout, run(&doc, &st, &paras, &records()).expect("reflow"));
}

/// Hostile input (V4-01/02): a run wider than the extent bound cannot fit at any width (width and
/// proportional kinds) and a story taller than the bound cannot fit at any height: both are refused
/// by the declared bound, never by looping. A canceled token stops the sizing.
#[test]
fn unfit_story_refuses_by_the_bound_and_sizing_honours_cancel() {
    let wide = "x";
    for kind in [AutoSizing::WidthOnly, AutoSizing::HeightAndWidth, AutoSizing::HeightAndWidthProportionally] {
        let (doc, st, _) = sized(wide, spec(0, 10.0, 10.0, 64.0, 100.0), kind);
        let glyph = ShapedParagraph::from_fixed_advance(wide, SourceSpan { start: 0, end: 1 }, MAX_AUTOSIZE_EXTENT_PT + 1.0, 12.0, 10.0).expect("glyph");
        assert_eq!(autosize(&doc, &st, &[glyph]), Err(LayoutError::AutosizeOverBound), "{kind:?}");
    }
    // 1400 one-letter words, one per 6 pt line: 10 + 1399 * 12 + 4 = 16802 pt > 15552.
    let tall = vec!["a"; 1400].join(" ");
    let (doc, st, paras) = sized(&tall, spec(0, 10.0, 10.0, 10.0, 100.0), AutoSizing::HeightOnly);
    assert_eq!(autosize(&doc, &st, &paras), Err(LayoutError::AutosizeOverBound));
    let (doc, st, paras) = sized("aaaa bbbb cccc dddd eeee ffff", spec(0, 10.0, 10.0, 64.0, 32.0), AutoSizing::HeightAndWidthProportionally);
    let stop = hsk_studio_accord::CancellationToken::default();
    stop.cancel();
    assert_eq!(autosize_with(&doc, &st, &paras, &stop), Err(LayoutError::Canceled));
}

// ---- slice 3: output ---------------------------------------------------------------------------------

/// The receipt names every resized frame with its size before and after; the prospective document
/// is the base with those bounds written (top-left kept), validated by Folio, and a fixed-size reflow
/// of it lays out the same lines. A stale base is refused; without auto-size there is no resize and
/// the document is the base.
#[test]
fn autosize_receipt_reports_every_resize_and_the_document_takes_the_bounds() {
    let cancel = hsk_studio_accord::CancellationToken::default();
    let (doc, st, paras) = sized("aaaa bbbb cccc dddd eeee ffff", spec(0, 10.0, 10.0, 64.0, 100.0), AutoSizing::HeightOnly);
    let p = autosize(&doc, &st, &paras).expect("auto-sized");
    let r = &p.autosized_frames()[0];
    assert_eq!((r.frame_id.as_str(), r.frame_index, r.auto_sizing), (id("SLYR", 1).as_str(), 0, AutoSizing::HeightOnly));
    assert_eq!(resized(&p), (64.0, 100.0, 64.0, 38.0));
    let snap = p.autosized_document(&doc, Budget::default(), &Unresolved, &cancel).expect("Folio-valid document");
    let mut expected = doc.clone();
    frame(&mut expected, 0).bounds.height = len(38.0);
    assert_eq!(snap.document(), &expected);
    let mut fixed = snap.document().clone();
    frame(&mut fixed, 0).auto_sizing_type = AutoSizing::Off;
    assert_eq!(run(&fixed, &st, &paras, &records()).expect("reflow"), p.layout);
    let mut stale = doc.clone();
    stale.revision += 1;
    assert_eq!(p.autosized_document(&stale, Budget::default(), &Unresolved, &cancel).unwrap_err().code, Code::StaleRevision);
    let mut other = doc.clone();
    other.layers[0].layer_id = id("SLYR", 9);
    assert_eq!(p.autosized_document(&other, Budget::default(), &Unresolved, &cancel).unwrap_err().code, Code::InvalidField);
    let (off_doc, st, paras) = sized("aaaa", spec(0, 10.0, 10.0, 64.0, 100.0), AutoSizing::Off);
    let off = autosize(&off_doc, &st, &paras).expect("off");
    assert!(off.autosized_frames().is_empty());
    assert_eq!(off.autosized_document(&off_doc, Budget::default(), &Unresolved, &cancel).expect("valid").document(), &off_doc);
}
