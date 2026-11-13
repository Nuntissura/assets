//! Lane SMB-FIX-NEGATIVE-SPACE-1 (STU-TYP-185; R60 attempt 3 probe finding
//! negative-space-places-lines-outside-the-frame, port of the validator probe
//! probe_negative_space_before_places_lines_above_the_frame): a negative paragraph gap lifts a
//! paragraph at most to its column's first baseline (and its line box at most to the inset top), on
//! every composer, after column balancing, under a smaller leading, below an above-line anchor and
//! across a thread; vertical justification keeps such a column inside the frame (review of
//! a7de5fcd, F1-F3); the lines that then do not fit are overset. STU-TYP-185 states no bounds for space before/after (UNKNOWN): the floor at the column
//! top is a Studio reading, InDesign parity UNVERIFIED (D5). Toy font as tests/indent_spacing.rs
//! (6 pt glyphs, line height 12, ascent 10, first baseline 10 pt below the frame top).
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    AutoSizing, Bounds, DocumentSchema, FirstBaseline, GeometryUnit, GraphSchema, LayerSchema, LayoutEdges, Length, NodeKind, NodeRef,
    PageSide, PageSpreadSchema, Parent, Payload, ProfileBinding, StoryContent, StoryDirection, StoryParagraph, StoryRange, StoryTextKind,
    StudioAnchorPosition, StudioAnchoredObject, StudioAnchoredSettings, StudioCompositionSettings, StudioDocument, StudioFrameFlowOptions,
    StudioLayer, StudioLayerGraph, StudioLayoutFlowRecords, StudioPage, StudioPageSpread, StudioTextAlign, StudioTextFrame, StudioTextStory,
    StudioTypeStyle, StudioTypeStyleRecords, StudioStyleComposition, TextStorySchema, TypeStyleSchema, TypeStyleScope, VerticalJustification,
};
use hsk_studio_layout::*;

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
fn edges(v: f64) -> LayoutEdges {
    LayoutEdges { top: len(v), right: len(v), bottom: len(v), left: len(v) }
}

/// One text frame on page 1 at the origin: width, height, columns, gutter.
fn document(w: f64, h: f64, cols: u32, gutter: f64) -> StudioDocument {
    let unassigned = || ProfileBinding::Unassigned(());
    let page = StudioPage { page_id: "page-1".into(), width: len(1000.0), height: len(1000.0), side: PageSide::RightHand, margins: edges(5.0), bleed: None, applied_trap_preset: None };
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision: 3,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: unassigned(),
        working_space_cmyk: unassigned(),
        working_space_gray: unassigned(),
        working_space_spot: unassigned(),
        blending_space: unassigned(),
        artboards: vec![],
        page_spreads: vec![StudioPageSpread {
            schema_id: PageSpreadSchema::V1,
            page_spread_id: id("SPGS", 1),
            name: "spread".into(),
            parent: Parent::Root(()),
            bounds: Bounds { x: len(0.0), y: len(0.0), width: len(1000.0), height: len(1000.0) },
            pages: vec![page],
        }],
        layers: vec![StudioLayer {
            schema_id: LayerSchema::V1,
            layer_id: id("SLYR", 1),
            name: "frame".into(),
            parent: Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: id("SPGS", 1) }),
            kind: "text".into(),
            payload: Payload::Text { story_id: id("STXT", 1) },
            visible: true,
            appearance: None,
            text_frame: Some(StudioTextFrame {
                page_id: "page-1".into(),
                bounds: Bounds { x: len(0.0), y: len(0.0), width: len(w), height: len(h) },
                inset_spacing: edges(0.0),
                text_column_count: cols,
                text_column_gutter: len(gutter),
                first_baseline_offset: FirstBaseline::Fixed,
                minimum_first_baseline_offset: len(10.0),
                vertical_justification: VerticalJustification::Top,
                auto_sizing_type: AutoSizing::Off,
                previous_text_frame: None,
                next_text_frame: None,
            }),
        }],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    }
}

/// A story of paragraphs `(text with its terminator, paragraph style n, line height, ascent)`; the
/// terminator is the last character of every piece but the last and is not shaped.
struct Story {
    story: StudioTextStory,
    shaped: Vec<ShapedParagraph>,
}
fn story_led(pieces: &[(&str, Option<u32>, f64, f64)]) -> Story {
    let text: String = pieces.iter().map(|p| p.0).collect();
    let (mut paragraphs, mut shaped, mut start) = (Vec::new(), Vec::new(), 0u64);
    for (i, (piece, style, height, ascent)) in pieces.iter().enumerate() {
        let end = start + piece.len() as u64;
        paragraphs.push(StoryParagraph { start, end, style_ref: style.map(|n| id("STYS", n)), paragraph_direction: None });
        let body = if i + 1 < pieces.len() { &piece[..piece.len() - 1] } else { piece };
        let source = SourceSpan { start, end: start + body.len() as u64 };
        shaped.push(ShapedParagraph::from_fixed_advance(&text, source, 6.0, *height, *ascent).unwrap());
        start = end;
    }
    let story = StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", 1),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.clone() },
        paragraphs,
        character_ranges: vec![StoryRange { start: 0, end: text.len() as u64, style_ref: None, applied_language: Some("en-US".into()) }],
    };
    Story { story, shaped }
}
fn story(pieces: &[(&str, Option<u32>)]) -> Story {
    story_led(&pieces.iter().map(|&(t, s)| (t, s, 12.0, 10.0)).collect::<Vec<_>>())
}

/// Paragraph styles 1..=n, left aligned, with these settings (an empty one states none).
fn styles(settings: &[StudioCompositionSettings]) -> StudioTypeStyleRecords {
    let mut r = StudioTypeStyleRecords::new(id("SDOC", 1));
    for (i, s) in settings.iter().enumerate() {
        let n = i as u32 + 1;
        r.styles.push(StudioTypeStyle {
            schema_id: TypeStyleSchema::V1,
            style_id: id("STYS", n),
            scope: TypeStyleScope::Paragraph,
            name: format!("style/{n}"),
            based_on: None,
            next_style: None,
            font_family: None,
            font_style: None,
            font_size: Some(len(12.0)),
            fill: None,
            line_height: None,
            letter_spacing: None,
            align: Some(StudioTextAlign::Left),
            grep_styles: Vec::new(),
        });
        if !s.is_empty() {
            r.style_composition.push(StudioStyleComposition { style_id: id("STYS", n), settings: s.clone() });
        }
    }
    r
}
/// Style 1 states nothing, style 2 has space before -1000 pt.
fn lifting() -> StudioTypeStyleRecords {
    styles(&[StudioCompositionSettings::default(), StudioCompositionSettings { space_before: Some(len(-1000.0)), ..Default::default() }])
}
fn flow() -> StudioLayoutFlowRecords {
    StudioLayoutFlowRecords::new(id("SDOC", 1))
}
fn run_with(doc: &StudioDocument, s: &Story, records: &StudioTypeStyleRecords, composer: Composer, flow: &StudioLayoutFlowRecords) -> Result<LayoutProposal, LayoutError> {
    let req = ReflowRequest {
        document: doc,
        story: &s.story,
        resolved_text: None,
        paragraphs: &s.shaped,
        obstacles: &[],
        composer,
        space: SpaceParams::default(),
        expected_revision: 3,
        limits: Limits::default(),
    };
    let opts = TypesetOptions { styles: Some(StyleSource { records, default_font_size_pt: 12.0 }), hyphen_advance_pt: Some(6.0), ..Default::default() };
    reflow_flow(&req, &opts, flow, &CancellationToken::default())
}
fn lines(p: &LayoutProposal) -> Vec<Line> {
    p.frames.iter().flat_map(|f| f.lines.clone()).collect()
}
fn composers() -> [Composer; 4] {
    [
        Composer::FirstFit,
        Composer::KnuthPlass(KnuthPlassParams::default()),
        Composer::AdobeSingleLine(AdobeParams::default()),
        Composer::AdobeParagraph(AdobeParams::default()),
    ]
}

/// The validator probe: 40 one-line paragraphs in a 120 x 100 pt frame, paragraph 2 with space
/// before -1000 pt. Paragraph 2 rises to the column's first baseline (10), not to 10 + 24 - 1000;
/// the following lines step down from there, and paragraphs 10..39 (byte 30 on) are overset.
#[test]
fn negative_space_before_never_lifts_a_line_above_the_column_top() {
    let mut pieces: Vec<(&str, Option<u32>)> = vec![("aa\n", Some(1)); 40];
    pieces[2] = ("bb\n", Some(2));
    pieces[39] = ("zz", Some(1));
    let s = story(&pieces);
    let doc = document(120.0, 100.0, 1, 0.0);
    for composer in composers() {
        let p = run_with(&doc, &s, &lifting(), composer, &flow()).unwrap();
        let ls = lines(&p);
        let top = ls[0].baseline_y_pt;
        let min = ls.iter().map(|l| l.baseline_y_pt).fold(f64::MAX, f64::min);
        assert!(min >= top - 1e-6, "{composer:?}: a line sits above the frame's first line (min {min}, first {top})");
        assert!(p.overset.is_some(), "{composer:?}: 40 lines in a 100 pt frame and no overset");
        let got: Vec<f64> = ls.iter().map(|l| l.baseline_y_pt).collect();
        assert_eq!(got, vec![10.0, 22.0, 10.0, 22.0, 34.0, 46.0, 58.0, 70.0, 82.0, 94.0], "{composer:?}");
        assert_eq!(p.overset.map(|o| (o.start, o.end)), Some((30, 119)), "{composer:?}");
    }
}

/// Column balancing re-adds a paragraph's gap where its first line no longer opens a column
/// (SMB-TYPE-INDENT-SPACING-1 review F2): a negative gap re-added there also stops at the column's
/// first baseline. Two 60 pt columns three lines deep; A takes column 0, B (space before -1000)
/// rises to column 0's top; balanced, B is row 1 of column 1 and rises to that column's top.
#[test]
fn balancing_lifts_a_negative_gap_at_most_to_the_column_top() {
    let s = story(&[("aaaaaaaa bbbbbbbb cccccccc\n", Some(1)), ("dd", Some(2))]);
    let doc = document(130.0, 36.0, 2, 10.0);
    let mut balanced = flow();
    balanced.frame_options.push(StudioFrameFlowOptions { layer_id: id("SLYR", 1), vertical_balance_columns: true });
    for composer in composers() {
        let ragged = run_with(&doc, &s, &lifting(), composer, &flow()).unwrap();
        let got: Vec<(usize, f64)> = lines(&ragged).iter().map(|l| (l.column, l.baseline_y_pt)).collect();
        assert_eq!(got, vec![(0, 10.0), (0, 22.0), (0, 34.0), (0, 10.0)], "{composer:?} ragged");
        let p = run_with(&doc, &s, &lifting(), composer, &balanced).unwrap();
        let got: Vec<(usize, f64)> = lines(&p).iter().map(|l| (l.column, l.baseline_y_pt)).collect();
        assert_eq!(got, vec![(0, 10.0), (0, 22.0), (1, 10.0), (1, 10.0)], "{composer:?} balanced");
        assert_eq!(p.overset, None, "{composer:?}");
    }
}

/// Under the mixed-leading reading of `baseline_of` (earlier lines of a column advance by the
/// current paragraph's line height) a line of smaller leading after a lifted paragraph would land
/// at 10 + 2 * 4 - 12 = 6: it stays on the column's first baseline.
#[test]
fn a_smaller_leading_after_a_negative_gap_stays_at_the_column_top() {
    let s = story_led(&[("aa\n", Some(1), 12.0, 10.0), ("bb\n", Some(2), 12.0, 10.0), ("cc", Some(1), 4.0, 3.0)]);
    let doc = document(120.0, 100.0, 1, 0.0);
    for composer in composers() {
        let p = run_with(&doc, &s, &lifting(), composer, &flow()).unwrap();
        let got: Vec<f64> = lines(&p).iter().map(|l| l.baseline_y_pt).collect();
        assert_eq!(got, vec![10.0, 10.0, 10.0], "{composer:?}");
    }
}

/// An above-line anchored object pushes its paragraph down by space above + height + y offset (4 +
/// 20 + 6 = 30, anchors module); the push starts where the negative gap left the paragraph (the
/// column top), so the line lands at 10 + 30, not at 10 + 24 - 1000 + 30.
#[test]
fn an_above_line_anchor_pushes_from_the_lifted_position() {
    let s = story(&[("aa\n", Some(1)), ("bb\n", Some(1)), ("cc", Some(2))]);
    let doc = document(120.0, 100.0, 1, 0.0);
    let mut fl = flow();
    let settings = StudioAnchoredSettings { anchored_position: StudioAnchorPosition::AboveLine, anchor_space_above: len(4.0), anchor_y_offset: len(6.0), ..Default::default() };
    fl.anchored_objects.push(StudioAnchoredObject { layer_id: id("SLYR", 2), story_id: id("STXT", 1), anchor_byte: 6, width: len(30.0), height: len(20.0), settings });
    for composer in composers() {
        let p = run_with(&doc, &s, &lifting(), composer, &fl).unwrap();
        let got: Vec<f64> = lines(&p).iter().map(|l| l.baseline_y_pt).collect();
        assert_eq!(got, vec![10.0, 22.0, 40.0], "{composer:?}");
    }
}

/// `doc` with its frame's vertical justification and first-baseline kind and minimum offset set.
fn framed(mut doc: StudioDocument, vjust: VerticalJustification, kind: FirstBaseline, min_first: f64) -> StudioDocument {
    let f = doc.layers[0].text_frame.as_mut().unwrap();
    f.vertical_justification = vjust;
    f.first_baseline_offset = kind;
    f.minimum_first_baseline_offset = len(min_first);
    doc
}
/// Two one-column `w` x `h` frames on page 1, threaded, the second 200 pt below the first.
fn threaded(w: f64, h: f64) -> StudioDocument {
    let mut doc = document(w, h, 1, 0.0);
    let mut second = doc.layers[0].clone();
    second.layer_id = id("SLYR", 2);
    second.name = "frame 2".into();
    let f = second.text_frame.as_mut().unwrap();
    f.bounds.y = len(200.0);
    f.previous_text_frame = Some(id("SLYR", 1));
    doc.layers[0].text_frame.as_mut().unwrap().next_text_frame = Some(id("SLYR", 2));
    doc.layers.push(second);
    doc
}
fn baselines(p: &LayoutProposal) -> Vec<f64> {
    lines(p).iter().map(|l| l.baseline_y_pt).collect()
}

/// Review F1 (a7de5fcd): vertical justification measures a column's block from its highest line top
/// to its deepest line bottom, not from its first and last lines in index order (a lifted paragraph
/// stacked on the column's first baseline is not the block's bottom). Bottom, Center and Justify
/// then keep every line box (ascent 10, descent 2) inside the 100 pt frame, and nothing is overset.
/// The column 10, 22, 34, 10 is the block [0, 36]: Bottom moves it by 64, not by 100 - 12 = 88.
#[test]
fn vertical_justification_keeps_every_line_of_a_lifted_column_inside_the_frame() {
    let four = story(&[("aa\n", Some(1)), ("bb\n", Some(1)), ("cc\n", Some(1)), ("dd", Some(2))]);
    let mut pieces: Vec<(&str, Option<u32>)> = vec![("aa\n", Some(1)); 8];
    pieces.push(("zz", Some(2)));
    let nine = story(&pieces);
    for vjust in [VerticalJustification::Bottom, VerticalJustification::Center, VerticalJustification::Justify] {
        let doc = framed(document(120.0, 100.0, 1, 0.0), vjust, FirstBaseline::Fixed, 10.0);
        for s in [&four, &nine] {
            for composer in composers() {
                let p = run_with(&doc, s, &lifting(), composer, &flow()).unwrap();
                let got = baselines(&p);
                assert!(got.iter().all(|b| b - 10.0 >= -1e-9 && b + 2.0 <= 100.0 + 1e-9), "{vjust:?} {composer:?}: {got:?}");
                assert_eq!(p.overset, None, "{vjust:?} {composer:?}");
            }
        }
    }
    let doc = framed(document(120.0, 100.0, 1, 0.0), VerticalJustification::Bottom, FirstBaseline::Fixed, 10.0);
    for composer in composers() {
        let p = run_with(&doc, &four, &lifting(), composer, &flow()).unwrap();
        assert_eq!(baselines(&p), vec![74.0, 86.0, 98.0, 74.0], "{composer:?}");
    }
}

/// Review F2: the floor bounds the line box, not only the baseline. Ascent first-baseline kind,
/// minimum 0: the column's first baseline is P0's ascent, 10. P1 (line height 24, ascent 20) lifted
/// by -1000 stops where its box top meets the inset top: baseline 20 (box [0, 24]), not 10 (box
/// [-10, 14]).
#[test]
fn a_lifted_paragraph_with_a_larger_ascent_keeps_its_line_box_inside_the_inset_top() {
    let s = story_led(&[("aa\n", Some(1), 12.0, 10.0), ("bb", Some(2), 24.0, 20.0)]);
    let doc = framed(document(120.0, 100.0, 1, 0.0), VerticalJustification::Top, FirstBaseline::Ascent, 0.0);
    for composer in composers() {
        let p = run_with(&doc, &s, &lifting(), composer, &flow()).unwrap();
        assert_eq!(baselines(&p), vec![10.0, 20.0], "{composer:?}");
        assert_eq!(p.overset, None, "{composer:?}");
    }
}

/// Review F3 (a), the next_slot floor on its own: the lines after a floored line step down from it.
/// P2 ("cc cc", line height 4, ascent 3) breaks into two lines in a 20 pt frame; its first line would
/// be 10 + 2 * 4 - 12 = 6 and is floored to 10, its second follows at 14. Without the next_slot
/// floor (the baseline_of guard alone) both would sit at 10.
#[test]
fn the_lines_after_a_floored_line_step_down_from_it() {
    let s = story_led(&[("aa\n", Some(1), 12.0, 10.0), ("bb\n", Some(2), 12.0, 10.0), ("cc cc", Some(1), 4.0, 3.0)]);
    let doc = document(20.0, 100.0, 1, 0.0);
    for composer in composers() {
        let p = run_with(&doc, &s, &lifting(), composer, &flow()).unwrap();
        assert_eq!(baselines(&p), vec![10.0, 10.0, 10.0, 14.0], "{composer:?}");
        assert_eq!(p.overset, None, "{composer:?}");
    }
}

/// Review F3 (b), the balance_band floor on its own: balanced over two 60 pt columns three lines
/// deep, column 1 holds A's fourth line, B (space before -1000) floored to 10, and C, which steps
/// down to 22. Without the balance_band floor (the baseline_of guard alone) C would sit at 10.
#[test]
fn balancing_steps_the_row_after_a_floored_row_down_from_it() {
    let s = story(&[("aaaaaaaa bbbbbbbb cccccccc dddddddd\n", Some(1)), ("bb\n", Some(2)), ("ee", Some(1))]);
    let doc = document(130.0, 36.0, 2, 10.0);
    let mut balanced = flow();
    balanced.frame_options.push(StudioFrameFlowOptions { layer_id: id("SLYR", 1), vertical_balance_columns: true });
    for composer in composers() {
        let p = run_with(&doc, &s, &lifting(), composer, &balanced).unwrap();
        let got: Vec<(usize, f64)> = lines(&p).iter().map(|l| (l.column, l.baseline_y_pt)).collect();
        assert_eq!(got, vec![(0, 10.0), (0, 22.0), (0, 34.0), (1, 10.0), (1, 10.0), (1, 22.0)], "{composer:?}");
        assert_eq!(p.overset, None, "{composer:?}");
    }
}

/// Review F3 (c), a thread of two 20 x 36 pt frames: P3 ("bb bb bb bb", space before -1000) rises to
/// frame 1's first baseline, steps down to its bottom and continues at the top of frame 2; P4 and P5
/// follow there and P6 (byte 27 on) is overset.
#[test]
fn a_lifted_paragraph_continues_into_the_next_frame_and_the_rest_is_overset() {
    let s = story(&[("aa\n", Some(1)), ("aa\n", Some(1)), ("aa\n", Some(1)), ("bb bb bb bb\n", Some(2)), ("cc\n", Some(1)), ("dd\n", Some(1)), ("ee", Some(1))]);
    let doc = threaded(20.0, 36.0);
    for composer in composers() {
        let p = run_with(&doc, &s, &lifting(), composer, &flow()).unwrap();
        let per_frame: Vec<Vec<f64>> = p.frames.iter().map(|f| f.lines.iter().map(|l| l.baseline_y_pt).collect()).collect();
        assert_eq!(per_frame, vec![vec![10.0, 22.0, 34.0, 10.0, 22.0, 34.0], vec![10.0, 22.0, 34.0]], "{composer:?}");
        assert_eq!(p.overset.map(|o| (o.start, o.end)), Some((27, 29)), "{composer:?}");
    }
}
