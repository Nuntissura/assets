//! Vertical justification (lane SMB-ID-PREPRESS, STU-LAY-121, STU-LAY-118 `vertical_threshold`,
//! STU-TYP-190). Toy font of `support_layout`: 6 pt advance, 12 pt line height, 10 pt ascent, fixed
//! first baseline 10 pt, no insets. Hand values: three one-line lines at baselines 10/22/34 occupy
//! the block [0, 36]; a 100 pt frame leaves 64 pt of slack. InDesign parity UNVERIFIED (D5).
mod support_layout;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use support_layout::*;

fn baselines(p: &LayoutProposal) -> Vec<f64> {
    p.frames[0].lines.iter().map(|l| l.baseline_y_pt).collect()
}

fn one_frame(w: f64, vj: VerticalJustification) -> StudioDocument {
    let mut layer = frame_layer(1, 1, spec(0, 10.0, 10.0, w, 100.0), None, None);
    layer.text_frame.as_mut().unwrap().vertical_justification = vj;
    document(vec![layer], [PageSide::RightHand, PageSide::LeftHand])
}

fn threshold(v: f64) -> StudioLayoutFlowRecords {
    let mut r = records();
    r.vertical_thresholds.push(StudioFrameVerticalThreshold {
        layer_id: id("SLYR", 1),
        vertical_threshold: len(v),
    });
    r
}

#[test]
fn vjust_center_and_bottom_move_the_block() {
    let text = "aa\nbb\ncc";
    let (st, paras) = story(1, text, &[(0, 2), (3, 5), (6, 8)]);
    let top = run(&one_frame(120.0, VerticalJustification::Top), &st, &paras, &records()).unwrap();
    assert_eq!(baselines(&top), vec![10.0, 22.0, 34.0]);
    // Bottom: the last line's bottom (34 + 2) moves onto 100: dy = 64.
    let bottom = run(&one_frame(120.0, VerticalJustification::Bottom), &st, &paras, &records()).unwrap();
    assert_eq!(baselines(&bottom), vec![74.0, 86.0, 98.0]);
    // Center: block height 36, free 64, new block top 32 (block top was 0): dy = 32.
    let center = run(&one_frame(120.0, VerticalJustification::Center), &st, &paras, &records()).unwrap();
    assert_eq!(baselines(&center), vec![42.0, 54.0, 66.0]);
    // Line breaks and x positions are untouched.
    for (a, b) in top.frames[0].lines.iter().zip(&center.frames[0].lines) {
        assert_eq!((a.x_start_pt, a.source, a.glyph_range.clone()), (b.x_start_pt, b.source, b.glyph_range.clone()));
    }
    assert!(center.overset.is_none());
}

#[test]
fn vjust_justify_without_limit_puts_the_slack_between_paragraphs() {
    // Width 30 = five 6 pt glyphs: paragraph 1 breaks into "aaaa " / "aaaa", paragraph 2 is "bb".
    let text = "aaaa aaaa\nbb";
    let (st, paras) = story(1, text, &[(0, 9), (10, 12)]);
    let doc = one_frame(30.0, VerticalJustification::Justify);
    let p = run(&doc, &st, &paras, &records()).unwrap();
    // One paragraph gap takes all 64 pt; the line gap inside paragraph 1 is unchanged.
    assert_eq!(baselines(&p), vec![10.0, 22.0, 98.0]);
    assert_eq!(p.frames[0].lines.iter().map(|l| l.paragraph).collect::<Vec<_>>(), vec![0, 0, 1]);
}

#[test]
fn vjust_justify_threshold_caps_the_paragraph_gap_and_spreads_the_rest_over_lines() {
    let text = "aaaa aaaa\nbb";
    let (st, paras) = story(1, text, &[(0, 9), (10, 12)]);
    let doc = one_frame(30.0, VerticalJustification::Justify);
    let flow = threshold(20.0);
    validate_layout_flow_records(&flow, &doc, &CancellationToken::default()).unwrap();
    // Paragraph gap capped at 20; the remaining 44 is shared by the 2 line gaps (22 each):
    // 22 + 22 = 44; 34 + 22 + 22 + 20 = 98.
    let p = run(&doc, &st, &paras, &flow).unwrap();
    assert_eq!(baselines(&p), vec![10.0, 44.0, 98.0]);
    // Threshold 0: every gap gets 64 / 2 = 32.
    let p0 = run(&doc, &st, &paras, &threshold(0.0)).unwrap();
    assert_eq!(baselines(&p0), vec![10.0, 54.0, 98.0]);
    // A single line is not moved by justify.
    let (one, one_p) = story(1, "aa", &[(0, 2)]);
    assert_eq!(baselines(&run(&doc, &one, &one_p, &flow).unwrap()), vec![10.0]);
}

#[test]
fn vjust_threshold_records_are_checked() {
    let doc = one_frame(30.0, VerticalJustification::Justify);
    let tok = CancellationToken::default();
    // Negative, wrong unit and non-finite limits are refused by Folio and by layout.
    let neg = threshold(-1.0);
    assert!(validate_layout_flow_records(&neg, &doc, &tok).is_err());
    let (st, paras) = story(1, "aaaa aaaa\nbb", &[(0, 9), (10, 12)]);
    assert_eq!(run(&doc, &st, &paras, &neg).unwrap_err(), LayoutError::InvalidGeometry);
    let nan = threshold(f64::NAN);
    assert!(validate_layout_flow_records(&nan, &doc, &tok).is_err());
    assert_eq!(run(&doc, &st, &paras, &nan).unwrap_err(), LayoutError::InvalidGeometry);
    let mut mm = threshold(5.0);
    mm.vertical_thresholds[0].vertical_threshold.unit = GeometryUnit::Mm;
    assert!(validate_layout_flow_records(&mm, &doc, &tok).is_err());
    // Dangling and duplicate frame keys.
    let mut dangling = threshold(5.0);
    dangling.vertical_thresholds[0].layer_id = id("SLYR", 9);
    assert!(validate_layout_flow_records(&dangling, &doc, &tok).is_err());
    let mut dup = threshold(5.0);
    dup.vertical_thresholds.push(dup.vertical_thresholds[0].clone());
    assert!(validate_layout_flow_records(&dup, &doc, &tok).is_err());
}

#[test]
fn vjust_columns_span_and_wrap() {
    // Two columns of 40 pt (gutter 10, width 90): "aaaa bbbb cccc" breaks into three 5-glyph-slot
    // lines; a 30 pt high frame holds two lines per column (baselines 10, 22): column 0 gets lines
    // 1-2, column 1 line 3. Bottom moves each column on its own: column 0 block [0, 24] -> dy 6,
    // column 1 block [0, 12] -> dy 18.
    let mut layer = frame_layer(1, 1, Spec { columns: 2, gutter: 10.0, ..spec(0, 10.0, 10.0, 90.0, 30.0) }, None, None);
    layer.text_frame.as_mut().unwrap().vertical_justification = VerticalJustification::Bottom;
    let doc = document(vec![layer], [PageSide::RightHand, PageSide::LeftHand]);
    let (st, paras) = story(1, "aaaa bbbb cccc", &[(0, 14)]);
    let p = run(&doc, &st, &paras, &records()).unwrap();
    let got: Vec<(usize, f64)> = p.frames[0].lines.iter().map(|l| (l.column, l.baseline_y_pt)).collect();
    assert_eq!(got, vec![(0, 16.0), (0, 28.0), (1, 28.0)]);
    // A justified frame holding a span paragraph is refused, not mis-laid.
    let mut layer = frame_layer(1, 1, Spec { columns: 2, gutter: 10.0, ..spec(0, 10.0, 10.0, 90.0, 100.0) }, None, None);
    layer.text_frame.as_mut().unwrap().vertical_justification = VerticalJustification::Justify;
    let doc = document(vec![layer], [PageSide::RightHand, PageSide::LeftHand]);
    let (st, paras) = story(1, "HEAD\naaaa", &[(0, 4), (5, 9)]);
    let mut flow = records();
    flow.paragraph_flows.push(StudioParagraphFlow {
        story_id: id("STXT", 1),
        paragraph_start: 0,
        span_column: StudioSpanColumn::SpanColumns,
        span_count: Some(StudioSpanCount::All),
        span_column_count: 1,
        split_column_count: 2,
        space_before: len(0.0),
        space_after: len(0.0),
        inside_gutter: len(0.0),
        outside_gutter: len(0.0),
    });
    assert_eq!(run(&doc, &st, &paras, &flow).unwrap_err(), LayoutError::UnsupportedComposition);
    // Wrap obstacles: the frame is left as composed (top).
    let mut layer = frame_layer(1, 1, spec(0, 10.0, 10.0, 120.0, 100.0), None, None);
    layer.text_frame.as_mut().unwrap().vertical_justification = VerticalJustification::Bottom;
    let doc = document(vec![layer], [PageSide::RightHand, PageSide::LeftHand]);
    let (st, paras) = story(1, "aa", &[(0, 2)]);
    let obstacles = [FrameObstacles { frame_id: id("SLYR", 1), rects: vec![Rect { x0_pt: 100.0, y0_pt: 90.0, x1_pt: 110.0, y1_pt: 95.0 }] }];
    let p = reflow_flow(
        &ReflowRequest {
            document: &doc,
            story: &st,
            resolved_text: None,
            paragraphs: &paras,
            obstacles: &obstacles,
            composer: Composer::FirstFit,
            space: SpaceParams::default(),
            expected_revision: 2,
            limits: Limits::default(),
        },
        &TypesetOptions::default(),
        &records(),
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(baselines(&p), vec![10.0]);
}
