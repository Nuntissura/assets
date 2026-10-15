//! Lane SMB-LAYOUT-UI slice 1: multi-column frames, balance, span/split paragraphs, threading.
//! Hand-computed oracles. Toy font: every glyph advances 6 pt, line height 12 pt, ascent 10 pt
//! (descent 2 pt), fixed first baseline 10 pt, no insets. A 60 pt column holds 10 glyphs, so
//! "aaaa bbbb " (54 pt + a 6 pt hanging space) is one line. Glyph x in spread coordinates =
//! frame origin x + line x_start + the advances before the glyph on its line.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    AutoSizing, Bounds, DocumentSchema, FirstBaseline, GeometryUnit,
    GraphSchema, LayerSchema, LayoutEdges, Length, NodeKind, NodeRef, PageSide, PageSpreadSchema,
    Parent, Payload, ProfileBinding, StoryContent, StoryDirection, StoryParagraph, StoryRange,
    StoryTextKind, StudioDocument, StudioFrameFlowOptions, StudioLayer, StudioLayerGraph,
    StudioLayoutFlowRecords, StudioPage, StudioPageSpread, StudioParagraphFlow, StudioSpanColumn,
    StudioSpanCount, StudioTextFrame, StudioTextStory, TextStorySchema, VerticalJustification,
    validate_layout_flow_records,
};
use hsk_studio_layout::*;

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn len(v: f64) -> Length {
    Length {
        value: v,
        unit: GeometryUnit::Pt,
    }
}
fn edges(v: f64) -> LayoutEdges {
    LayoutEdges {
        top: len(v),
        right: len(v),
        bottom: len(v),
        left: len(v),
    }
}
fn page(key: &str) -> StudioPage {
    StudioPage {
        page_id: key.into(),
        width: len(200.0),
        height: len(200.0),
        side: PageSide::RightHand,
        margins: edges(5.0),
        bleed: None,
    }
}
/// (page, x, y, w, h, columns, gutter)
type Spec = (usize, f64, f64, f64, f64, u32, f64);
fn frame_layer(n: u32, s: &Spec, previous: Option<u32>, next: Option<u32>) -> StudioLayer {
    StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", n),
        name: format!("frame {n}"),
        parent: Parent::Node(NodeRef {
            node_kind: NodeKind::PageSpread,
            node_id: id("SPGS", 1),
        }),
        kind: "text".into(),
        payload: Payload::Text {
            story_id: id("STXT", 1),
        },
        visible: true,
        appearance: None,
        text_frame: Some(StudioTextFrame {
            page_id: format!("page-{}", s.0 + 1),
            bounds: Bounds {
                x: len(s.1),
                y: len(s.2),
                width: len(s.3),
                height: len(s.4),
            },
            inset_spacing: edges(0.0),
            text_column_count: s.5,
            text_column_gutter: len(s.6),
            first_baseline_offset: FirstBaseline::Fixed,
            minimum_first_baseline_offset: len(10.0),
            vertical_justification: VerticalJustification::Top,
            auto_sizing_type: AutoSizing::Off,
            previous_text_frame: previous.map(|p| id("SLYR", p)),
            next_text_frame: next.map(|q| id("SLYR", q)),
        }),
    }
}
fn document(specs: &[Spec]) -> StudioDocument {
    let n = specs.len() as u32;
    let unassigned = || ProfileBinding::Unassigned(());
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision: 2,
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
            bounds: Bounds {
                x: len(0.0),
                y: len(0.0),
                width: len(400.0),
                height: len(200.0),
            },
            pages: vec![page("page-1"), page("page-2")],
        }],
        layers: specs
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let k = i as u32 + 1;
                frame_layer(k, s, (k > 1).then_some(k - 1), (k < n).then_some(k + 1))
            })
            .collect(),
        graph: StudioLayerGraph {
            schema_id: GraphSchema::V1,
            operations: vec![],
            edges: vec![],
            outputs: vec![],
        },
        sequences: vec![],
    }
}
/// Story of `text` whose paragraphs are the given byte spans (shaped spans exclude the newline).
fn story(text: &str, paras: &[(u64, u64)]) -> (StudioTextStory, Vec<ShapedParagraph>) {
    let end = text.len() as u64;
    let st = StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", 1),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline {
            text_utf8: text.into(),
        },
        paragraphs: vec![StoryParagraph {
            start: 0,
            end,
            style_ref: None,
            paragraph_direction: None,
        }],
        character_ranges: vec![StoryRange {
            start: 0,
            end,
            style_ref: None,
            applied_language: None,
        }],
    };
    let shaped = paras
        .iter()
        .map(|&(s, e)| {
            ShapedParagraph::from_fixed_advance(text, SourceSpan { start: s, end: e }, 6.0, 12.0, 10.0)
                .unwrap()
        })
        .collect();
    (st, shaped)
}
fn run(
    doc: &StudioDocument,
    st: &StudioTextStory,
    paras: &[ShapedParagraph],
    flow: &StudioLayoutFlowRecords,
    obstacles: &[FrameObstacles],
) -> Result<LayoutProposal, LayoutError> {
    reflow_flow(
        &ReflowRequest {
            document: doc,
            story: st,
            resolved_text: None,
            paragraphs: paras,
            obstacles,
            composer: Composer::FirstFit,
            space: SpaceParams::default(),
            expected_revision: 2,
            limits: Limits::default(),
        },
        &TypesetOptions::default(),
        flow,
        &CancellationToken::default(),
    )
}
fn records() -> StudioLayoutFlowRecords {
    StudioLayoutFlowRecords::new(id("SDOC", 1))
}
fn pflow(start: u64, kind: StudioSpanColumn) -> StudioParagraphFlow {
    StudioParagraphFlow {
        story_id: id("STXT", 1),
        paragraph_start: start,
        span_column: kind,
        span_count: Some(StudioSpanCount::All),
        span_column_count: 1,
        split_column_count: 2,
        space_before: len(0.0),
        space_after: len(0.0),
        inside_gutter: len(0.0),
        outside_gutter: len(0.0),
    }
}
/// (frame, column, baseline, x_start, source start..end) of every placed line, in flow order.
fn lines(p: &LayoutProposal) -> Vec<(usize, usize, f64, f64, u64, u64)> {
    p.frames
        .iter()
        .enumerate()
        .flat_map(|(fi, f)| {
            f.lines
                .iter()
                .map(move |l| (fi, l.column, l.baseline_y_pt, l.x_start_pt, l.source.start, l.source.end))
        })
        .collect()
}
/// Spread x of the glyph whose cluster starts at `byte` (6 pt per glyph before it on its line).
fn glyph_x(p: &LayoutProposal, byte: u64) -> (f64, f64) {
    for f in &p.frames {
        for l in &f.lines {
            if l.source.start <= byte && byte < l.source.end {
                let before = (byte - l.source.start) as f64 * 6.0;
                return (f.origin_x_pt + l.x_start_pt + before, f.origin_y_pt + l.baseline_y_pt);
            }
        }
    }
    panic!("byte {byte} not placed");
}

const TEN_WORDS: &str = "aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii jjjj";

#[test]
fn columns_and_gutter_then_thread_to_next_page() {
    // Frame 1: page 1, 130 x 24, two 60 pt columns with a 10 pt gutter (two lines each).
    // Frame 2: page 2 at x 10, one 60 x 24 column. Five lines: col 0, col 1, then frame 2.
    let doc = document(&[(0, 0.0, 0.0, 130.0, 24.0, 2, 10.0), (1, 10.0, 0.0, 60.0, 24.0, 1, 0.0)]);
    let (st, paras) = story(TEN_WORDS, &[(0, 49)]);
    let p = run(&doc, &st, &paras, &records(), &[]).unwrap();
    assert_eq!(p.overset, None);
    assert_eq!(
        lines(&p),
        vec![
            (0, 0, 10.0, 0.0, 0, 10),
            (0, 0, 22.0, 0.0, 10, 20),
            (0, 1, 10.0, 70.0, 20, 30),
            (0, 1, 22.0, 70.0, 30, 40),
            (1, 0, 10.0, 0.0, 40, 49),
        ]
    );
    // Glyph positions: 'e' opens column 1 (x 70), 'f' 5 glyphs later, 'i' on page 2 (x 200 + 10).
    assert_eq!(glyph_x(&p, 20), (70.0, 10.0));
    assert_eq!(glyph_x(&p, 25), (100.0, 10.0));
    assert_eq!(glyph_x(&p, 35), (100.0, 22.0));
    // Pages are 200 pt wide: page 2 starts at spread x 200, frame 2 at 210.
    assert_eq!(glyph_x(&p, 40), (210.0, 10.0));
    assert!(p.frames[0].lines.iter().all(|l| l.available_pt == 60.0));
    // Plain reflow composes the same columns (records only add span/split and balancing).
    let plain = reflow(
        &ReflowRequest {
            document: &doc,
            story: &st,
            resolved_text: None,
            paragraphs: &paras,
            obstacles: &[],
            composer: Composer::FirstFit,
            space: SpaceParams::default(),
            expected_revision: 2,
            limits: Limits::default(),
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(plain, p);
    // Seven lines for six slots (frame 2 holds two): overset is exactly the seventh line.
    let text = format!("{TEN_WORDS} kkkk llll mmmm nnnn");
    let (st, paras) = story(&text, &[(0, text.len() as u64)]);
    let over = run(&doc, &st, &paras, &records(), &[]).unwrap();
    assert_eq!(over.overset, Some(SourceSpan { start: 60, end: 69 }));
    assert_eq!(over.frames[1].lines.len(), 2);
}

#[test]
fn balanced_columns_at_story_end() {
    // One 130 x 48 frame, two columns of four lines; five lines of text.
    let doc = document(&[(0, 0.0, 0.0, 130.0, 48.0, 2, 10.0)]);
    let (st, paras) = story(TEN_WORDS, &[(0, 49)]);
    let ragged = run(&doc, &st, &paras, &records(), &[]).unwrap();
    let cols: Vec<usize> = lines(&ragged).iter().map(|l| l.1).collect();
    assert_eq!(cols, vec![0, 0, 0, 0, 1]);
    let mut flow = records();
    flow.frame_options.push(StudioFrameFlowOptions {
        layer_id: id("SLYR", 1),
        vertical_balance_columns: true,
    });
    let balanced = run(&doc, &st, &paras, &flow, &[]).unwrap();
    // k = ceil(5 / 2) = 3: lines 0..3 in column 0, lines 3..5 in column 1; breaks unchanged.
    assert_eq!(
        lines(&balanced),
        vec![
            (0, 0, 10.0, 0.0, 0, 10),
            (0, 0, 22.0, 0.0, 10, 20),
            (0, 0, 34.0, 0.0, 20, 30),
            (0, 1, 10.0, 70.0, 30, 40),
            (0, 1, 22.0, 70.0, 40, 49),
        ]
    );
    assert_eq!(glyph_x(&balanced, 30), (70.0, 10.0));
    // A frame with a wrap obstacle is not balanced (documented), never mis-laid.
    let obstacles = vec![FrameObstacles {
        frame_id: id("SLYR", 1),
        rects: vec![Rect { x0_pt: 120.0, y0_pt: 40.0, x1_pt: 130.0, y1_pt: 48.0 }],
    }];
    let unbalanced = run(&doc, &st, &paras, &flow, &obstacles).unwrap();
    assert_eq!(lines(&unbalanced).iter().map(|l| l.1).collect::<Vec<_>>(), vec![0, 0, 0, 0, 1]);
}

#[test]
fn span_heading_and_split_paragraph() {
    // Span: "HEAD" spans both columns at the frame top (baseline 10, x 0, 130 wide); with 6 pt
    // space after, the body band starts at 10 + 2 + 6 = 18, so body baselines are 28, 40, 52.
    // With balancing, the three body lines split 2 + 1 under the head.
    let text = "HEAD\naaaa bbbb cccc dddd eeee ffff";
    let doc = document(&[(0, 0.0, 0.0, 130.0, 60.0, 2, 10.0)]);
    let (st, paras) = story(text, &[(0, 4), (5, 34)]);
    let mut flow = records();
    let mut head = pflow(0, StudioSpanColumn::SpanColumns);
    head.space_after = len(6.0);
    flow.paragraph_flows.push(head.clone());
    let p = run(&doc, &st, &paras, &flow, &[]).unwrap();
    assert_eq!(
        lines(&p),
        vec![
            (0, 0, 10.0, 0.0, 0, 4),
            (0, 0, 28.0, 0.0, 5, 15),
            (0, 0, 40.0, 0.0, 15, 25),
            (0, 0, 52.0, 0.0, 25, 34),
        ]
    );
    assert_eq!(p.frames[0].lines[0].available_pt, 130.0);
    flow.frame_options.push(StudioFrameFlowOptions {
        layer_id: id("SLYR", 1),
        vertical_balance_columns: true,
    });
    let b = run(&doc, &st, &paras, &flow, &[]).unwrap();
    assert_eq!(
        lines(&b),
        vec![
            (0, 0, 10.0, 0.0, 0, 4),
            (0, 0, 28.0, 0.0, 5, 15),
            (0, 0, 40.0, 0.0, 15, 25),
            (0, 1, 28.0, 70.0, 25, 34),
        ]
    );
    assert_eq!(glyph_x(&b, 25), (70.0, 28.0));

    // Split: one 128-wide column, 2 sub-columns, inside 4, outside 2 -> sub width 60. Three lines
    // fill ceil(3/2) = 2 rows column-major: (x 2, 10), (x 2, 22), (x 66, 10). The next paragraph
    // continues below the block at line slot 2 (baseline 34, x 0).
    let text = "aaaa bbbb cccc dddd eeee ffff\ngggg";
    let doc = document(&[(0, 0.0, 0.0, 128.0, 60.0, 1, 0.0)]);
    let (st, paras) = story(text, &[(0, 29), (30, 34)]);
    let mut flow = records();
    let mut split = pflow(0, StudioSpanColumn::SplitColumns);
    split.inside_gutter = len(4.0);
    split.outside_gutter = len(2.0);
    flow.paragraph_flows.push(split);
    let s = run(&doc, &st, &paras, &flow, &[]).unwrap();
    assert_eq!(
        lines(&s),
        vec![
            (0, 0, 10.0, 2.0, 0, 10),
            (0, 0, 22.0, 2.0, 10, 20),
            (0, 0, 10.0, 66.0, 20, 29),
            (0, 0, 34.0, 0.0, 30, 34),
        ]
    );
    assert!(s.frames[0].lines[..3].iter().all(|l| l.available_pt == 60.0));
    assert_eq!(glyph_x(&s, 25), (96.0, 10.0));
}

#[test]
fn hostile_threads_columns_and_overrides_refused() {
    let specs = [(0, 0.0, 0.0, 130.0, 24.0, 2, 10.0), (1, 0.0, 0.0, 130.0, 24.0, 2, 10.0)];
    let (st, paras) = story(TEN_WORDS, &[(0, 49)]);
    // Threading cycle: frame 1 <-> frame 2 with no start frame.
    let mut cyc = document(&specs);
    cyc.layers[0].text_frame.as_mut().unwrap().previous_text_frame = Some(id("SLYR", 2));
    cyc.layers[1].text_frame.as_mut().unwrap().next_text_frame = Some(id("SLYR", 1));
    assert_eq!(run(&cyc, &st, &paras, &records(), &[]), Err(LayoutError::CyclicThread));
    // Self-loop on a single frame.
    let mut own = document(&specs[..1]);
    let f = own.layers[0].text_frame.as_mut().unwrap();
    f.previous_text_frame = Some(id("SLYR", 1));
    f.next_text_frame = Some(id("SLYR", 1));
    assert_eq!(run(&own, &st, &paras, &records(), &[]), Err(LayoutError::CyclicThread));
    // Hostile column counts: refused before any allocation by column.
    let mut many = document(&specs);
    many.layers[0].text_frame.as_mut().unwrap().text_column_count = u32::MAX;
    assert_eq!(run(&many, &st, &paras, &records(), &[]), Err(LayoutError::BudgetExceeded));
    let mut zero = document(&specs);
    zero.layers[0].text_frame.as_mut().unwrap().text_column_count = 0;
    assert_eq!(run(&zero, &st, &paras, &records(), &[]), Err(LayoutError::InvalidGeometry));
    let doc = document(&specs);
    // Records of another document, overrides that name no paragraph, partial spans.
    let other = StudioLayoutFlowRecords::new(id("SDOC", 9));
    assert_eq!(run(&doc, &st, &paras, &other, &[]), Err(LayoutError::InvalidInput));
    let mut dangling = records();
    dangling.paragraph_flows.push(pflow(3, StudioSpanColumn::SpanColumns));
    assert_eq!(run(&doc, &st, &paras, &dangling, &[]), Err(LayoutError::InvalidInput));
    let mut three = document(&specs);
    three.layers[0].text_frame.as_mut().unwrap().text_column_count = 3;
    let mut partial = records();
    let mut two = pflow(0, StudioSpanColumn::SpanColumns);
    two.span_count = None;
    two.span_column_count = 2;
    partial.paragraph_flows.push(two);
    assert_eq!(run(&three, &st, &paras, &partial, &[]), Err(LayoutError::UnsupportedComposition));
    // Split sub-columns without positive width, and split into a frame with wrap obstacles.
    let mut wide = records();
    let mut s = pflow(0, StudioSpanColumn::SplitColumns);
    s.outside_gutter = len(40.0);
    wide.paragraph_flows.push(s);
    assert_eq!(run(&doc, &st, &paras, &wide, &[]), Err(LayoutError::InvalidGeometry));
    let mut split = records();
    split.paragraph_flows.push(pflow(0, StudioSpanColumn::SplitColumns));
    let obstacles = vec![FrameObstacles {
        frame_id: id("SLYR", 1),
        rects: vec![Rect { x0_pt: 0.0, y0_pt: 0.0, x1_pt: 5.0, y1_pt: 5.0 }],
    }];
    assert_eq!(
        run(&doc, &st, &paras, &split, &obstacles),
        Err(LayoutError::UnsupportedComposition)
    );
    let mut neg = records();
    let mut n = pflow(0, StudioSpanColumn::SpanColumns);
    n.space_after = len(-1.0);
    neg.paragraph_flows.push(n);
    assert_eq!(run(&doc, &st, &paras, &neg, &[]), Err(LayoutError::InvalidGeometry));
}

#[test]
fn flow_records_validate() {
    let doc = document(&[(0, 0.0, 0.0, 130.0, 24.0, 2, 10.0)]);
    let token = CancellationToken::default();
    let mut r = records();
    r.frame_options.push(StudioFrameFlowOptions {
        layer_id: id("SLYR", 1),
        vertical_balance_columns: true,
    });
    r.paragraph_flows.push(pflow(0, StudioSpanColumn::SpanColumns));
    validate_layout_flow_records(&r, &doc, &token).unwrap();
    assert_eq!(r.paragraph_flow(&id("STXT", 1), 0).map(|p| p.span_column), Some(StudioSpanColumn::SpanColumns));
    let code = |r: &StudioLayoutFlowRecords| validate_layout_flow_records(r, &doc, &token).unwrap_err().code;
    let mut dup = r.clone();
    dup.frame_options.push(dup.frame_options[0].clone());
    assert_eq!(code(&dup), hsk_studio_folio::Code::DuplicateIdentity);
    let mut dangling = r.clone();
    dangling.frame_options[0].layer_id = id("SLYR", 7);
    assert_eq!(code(&dangling), hsk_studio_folio::Code::DanglingReference);
    let mut split = r.clone();
    split.paragraph_flows[0].split_column_count = 1;
    assert_eq!(code(&split), hsk_studio_folio::Code::InvalidField);
    let mut unit = r.clone();
    unit.paragraph_flows[0].space_before = Length { value: 1.0, unit: GeometryUnit::Mm };
    assert_eq!(code(&unit), hsk_studio_folio::Code::InvalidUnit);
    let mut count = r.clone();
    count.paragraph_flows[0].span_column_count = 0;
    assert_eq!(code(&count), hsk_studio_folio::Code::InvalidField);
    let mut other = r.clone();
    other.document_id = id("SDOC", 2);
    assert_eq!(code(&other), hsk_studio_folio::Code::InvalidField);
}
