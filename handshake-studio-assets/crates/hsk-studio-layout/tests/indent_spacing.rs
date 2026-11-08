//! Lane SMB-TYPE-INDENT-SPACING-1 (STU-TYP-185): paragraph indents and spacing from the Folio
//! composition settings, composed by every composer. Toy font (every glyph 6 pt, line height 12,
//! ascent 10, first baseline 10 pt below the frame top, as tests/composer_wiring.rs); hand-computed
//! expectations. The rules (left / right / first-line indent narrow every slot where it is collected,
//! the last-line indent narrows the last line with one bounded re-break, space before and after add
//! one gap that a column top drops, `space_between_same_style`, span and split take the larger of
//! flow and attribute space) are Studio readings; InDesign parity is UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    AutoSizing, Bounds, DocumentSchema, FirstBaseline, GeometryUnit, GraphSchema, LayerSchema, LayoutEdges, Length, NodeKind, NodeRef,
    PageSide, PageSpreadSchema, Parent, Payload, ProfileBinding, StoryContent, StoryDirection, StoryParagraph, StoryRange, StoryTextKind,
    StudioCompositionScope, StudioCompositionSettings, StudioCrossReference, StudioDocument, StudioFrameFlowOptions, StudioLayer, StudioLayerGraph,
    StudioLayoutFlowRecords, StudioPage, StudioPageSpread, StudioParagraphComposition, StudioParagraphFlow, StudioSpanColumn,
    StudioSpanCount, StudioStyleComposition, StudioTextAlign, StudioTextFrame, StudioTextStory, StudioTypeStyle, StudioTypeStyleRecords,
    StudioXrefDestination, TextStorySchema, TypeStyleSchema, TypeStyleScope, VerticalJustification,
};
use hsk_studio_layout::*;

const COLUMN_BREAK: char = '\u{FDD8}';

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
fn pt(v: f64) -> Option<Length> {
    Some(len(v))
}
fn edges(v: f64) -> LayoutEdges {
    LayoutEdges { top: len(v), right: len(v), bottom: len(v), left: len(v) }
}

/// One text frame on page 1 at the origin.
#[derive(Clone, Copy)]
struct F {
    w: f64,
    h: f64,
    cols: u32,
    gutter: f64,
    auto: AutoSizing,
}
const fn frame(w: f64, h: f64) -> F {
    F { w, h, cols: 1, gutter: 0.0, auto: AutoSizing::Off }
}
const fn columns(w: f64, h: f64, cols: u32, gutter: f64) -> F {
    F { w, h, cols, gutter, auto: AutoSizing::Off }
}

fn document(f: F) -> StudioDocument {
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
                bounds: Bounds { x: len(0.0), y: len(0.0), width: len(f.w), height: len(f.h) },
                inset_spacing: edges(0.0),
                text_column_count: f.cols,
                text_column_gutter: len(f.gutter),
                first_baseline_offset: FirstBaseline::Fixed,
                minimum_first_baseline_offset: len(10.0),
                vertical_justification: VerticalJustification::Top,
                auto_sizing_type: f.auto,
                previous_text_frame: None,
                next_text_frame: None,
            }),
        }],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    }
}

/// A story of paragraphs `(text with its terminator, paragraph style n)`; the terminator (`\n` or a
/// break sentinel) is the last character of every piece but the last and is not shaped.
struct Story {
    story: StudioTextStory,
    shaped: Vec<ShapedParagraph>,
}
fn story(pieces: &[(&str, Option<u32>)]) -> Story {
    let text: String = pieces.iter().map(|p| p.0).collect();
    let (mut paragraphs, mut shaped, mut start) = (Vec::new(), Vec::new(), 0u64);
    for (i, (piece, style)) in pieces.iter().enumerate() {
        let end = start + piece.len() as u64;
        paragraphs.push(StoryParagraph { start, end, style_ref: style.map(|n| id("STYS", n)), paragraph_direction: None });
        let body = if i + 1 < pieces.len() { &piece[..piece.len() - piece.chars().next_back().map_or(0, char::len_utf8)] } else { piece };
        let source = SourceSpan { start, end: start + body.len() as u64 };
        shaped.push(ShapedParagraph::from_fixed_advance(&text, source, 6.0, 12.0, 10.0).unwrap());
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
/// One paragraph in style 1.
fn one(text: &str) -> Story {
    story(&[(text, Some(1))])
}

fn style(n: u32, align: StudioTextAlign) -> StudioTypeStyle {
    StudioTypeStyle {
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
        align: Some(align),
        grep_styles: Vec::new(),
    }
}
/// Paragraph styles 1..=n with these settings (an empty one states none).
fn styles_aligned(align: StudioTextAlign, settings: &[StudioCompositionSettings]) -> StudioTypeStyleRecords {
    let mut r = StudioTypeStyleRecords::new(id("SDOC", 1));
    for (i, s) in settings.iter().enumerate() {
        let n = i as u32 + 1;
        r.styles.push(style(n, align));
        if !s.is_empty() {
            r.style_composition.push(StudioStyleComposition { style_id: id("STYS", n), settings: s.clone() });
        }
    }
    r
}
fn styles(settings: &[StudioCompositionSettings]) -> StudioTypeStyleRecords {
    styles_aligned(StudioTextAlign::Left, settings)
}
fn opts(records: &StudioTypeStyleRecords) -> TypesetOptions<'_> {
    TypesetOptions { styles: Some(StyleSource { records, default_font_size_pt: 12.0 }), hyphen_advance_pt: Some(6.0), ..Default::default() }
}
fn flow() -> StudioLayoutFlowRecords {
    StudioLayoutFlowRecords::new(id("SDOC", 1))
}
fn request<'a>(doc: &'a StudioDocument, s: &'a Story, obstacles: &'a [FrameObstacles], composer: Composer) -> ReflowRequest<'a> {
    ReflowRequest {
        document: doc,
        story: &s.story,
        resolved_text: None,
        paragraphs: &s.shaped,
        obstacles,
        composer,
        space: SpaceParams::default(),
        expected_revision: 3,
        limits: Limits::default(),
    }
}
fn run_with(f: F, s: &Story, records: &StudioTypeStyleRecords, composer: Composer, flow: &StudioLayoutFlowRecords, obstacles: &[FrameObstacles]) -> Result<LayoutProposal, LayoutError> {
    let doc = document(f);
    reflow_flow(&request(&doc, s, obstacles, composer), &opts(records), flow, &CancellationToken::default())
}
fn run(f: F, s: &Story, records: &StudioTypeStyleRecords, composer: Composer) -> Result<LayoutProposal, LayoutError> {
    run_with(f, s, records, composer, &flow(), &[])
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
fn indents(left: f64, right: f64, first: f64) -> StudioCompositionSettings {
    StudioCompositionSettings { left_indent: pt(left), right_indent: pt(right), first_line_indent: pt(first), ..Default::default() }
}
fn space(before: f64, after: f64) -> StudioCompositionSettings {
    StudioCompositionSettings { space_before: pt(before), space_after: pt(after), ..Default::default() }
}
fn finite(p: &LayoutProposal) -> bool {
    lines(p).iter().all(|l| [l.baseline_y_pt, l.x_start_pt, l.width_pt, l.available_pt, l.hang_pt].iter().all(|v| v.is_finite()))
}

const SIX: &str = "aaaa bbbb cccc dddd eeee ffff";

/// Settings holding only zero indents and spacing compose bit-identically to no settings, on every
/// composer, aligned left and justified, in a balanced two-column frame (the token rule of
/// STU-TYP-130: these fields add inputs, they change no existing output).
#[test]
fn zero_indents_and_spacing_compose_bit_identically() {
    let zero = StudioCompositionSettings {
        left_indent: pt(0.0),
        right_indent: pt(-0.0),
        first_line_indent: pt(0.0),
        last_line_indent: pt(0.0),
        space_before: pt(0.0),
        space_after: pt(-0.0),
        space_between_same_style: Some(true),
        ..Default::default()
    };
    let text = "aaaa bbbb cccc dddd eeee ffff gggg\nhh ii jj kk ll mm nn oo pp\nqq rrrr ss tttt uu\n";
    let pieces: Vec<(&str, Option<u32>)> = text.split_inclusive('\n').map(|p| (p, Some(1))).collect();
    let s = story(&pieces);
    let mut balanced = flow();
    balanced.frame_options.push(StudioFrameFlowOptions { layer_id: id("SLYR", 1), vertical_balance_columns: true });
    for align in [StudioTextAlign::Left, StudioTextAlign::Justified, StudioTextAlign::Right] {
        for composer in composers() {
            let none = styles_aligned(align, &[StudioCompositionSettings::default()]);
            let zeros = styles_aligned(align, std::slice::from_ref(&zero));
            for f in [frame(100.0, 300.0), columns(130.0, 300.0, 2, 10.0)] {
                for fl in [flow(), balanced.clone()] {
                    let a = run_with(f, &s, &none, composer, &fl, &[]).unwrap();
                    let b = run_with(f, &s, &zeros, composer, &fl, &[]).unwrap();
                    assert_eq!(format!("{a:?}"), format!("{b:?}"), "{align:?} {composer:?}");
                }
            }
            // Zero settings next to a composer setting change nothing the composer setting does.
            let scope = StudioCompositionSettings { composition_scope: Some(StudioCompositionScope::EveryLine), ..Default::default() };
            let with = styles_aligned(align, &[scope.overlaid(&zero)]);
            let a = run(frame(100.0, 300.0), &s, &styles_aligned(align, &[scope]), composer).unwrap();
            let b = run(frame(100.0, 300.0), &s, &with, composer).unwrap();
            assert_eq!(format!("{a:?}"), format!("{b:?}"), "{align:?} {composer:?} with a scope");
        }
    }
}

/// Left, right and first-line indents narrow every slot: x moves by the left indent (plus the
/// first-line indent on the paragraph's first line) and the width loses both and the right indent;
/// lines break at the narrowed measure. A negative first-line indent hangs the first line.
#[test]
fn left_right_and_first_line_indents_narrow_every_line() {
    let s = one(SIX);
    let r = styles(&[indents(12.0, 6.0, 12.0)]);
    let p = run(frame(120.0, 100.0), &s, &r, Composer::FirstFit).unwrap();
    let got: Vec<(f64, f64, f64, u64)> = lines(&p).iter().map(|l| (l.x_start_pt, l.available_pt, l.width_pt, l.source.start)).collect();
    // Line 0: x 24, measure 90, "aaaa bbbb cccc" (84); line 1: x 12, measure 102.
    assert_eq!(got, vec![(24.0, 90.0, 84.0, 0), (12.0, 102.0, 84.0, 15)]);
    // Hanging first line.
    let hang = run(frame(120.0, 100.0), &s, &styles(&[indents(18.0, 0.0, -18.0)]), Composer::FirstFit).unwrap();
    let got: Vec<(f64, f64, u64)> = lines(&hang).iter().map(|l| (l.x_start_pt, l.available_pt, l.source.start)).collect();
    assert_eq!(got, vec![(0.0, 120.0, 0), (18.0, 102.0, 20)]);
    // Every composer sets every line inside its narrowed slot.
    for composer in composers() {
        let p = run(frame(120.0, 100.0), &s, &r, composer).unwrap();
        for (k, l) in lines(&p).iter().enumerate() {
            let (x, w) = if k == 0 { (24.0, 90.0) } else { (12.0, 102.0) };
            assert_eq!((l.x_start_pt, l.available_pt), (x, w), "{composer:?} line {k}");
            assert!(!l.overfull && l.width_pt <= w + 1e-9, "{composer:?} line {k}: {l:?}");
        }
    }
}

/// With wrap obstacles the indents measure from the free gap; a slot the indents leave without
/// width is skipped like a baseline without a gap, and the next slot is still the first line.
#[test]
fn indents_measure_from_the_free_gap_and_skip_slots_without_width() {
    let s = one(SIX);
    let r = styles(&[indents(6.0, 0.0, 0.0)]);
    let left_block = [FrameObstacles { frame_id: id("SLYR", 1), rects: vec![Rect { x0_pt: 0.0, y0_pt: 0.0, x1_pt: 30.0, y1_pt: 12.0 }] }];
    let p = run_with(frame(120.0, 100.0), &s, &r, Composer::FirstFit, &flow(), &left_block).unwrap();
    let got: Vec<(f64, f64, f64)> = lines(&p).iter().map(|l| (l.baseline_y_pt, l.x_start_pt, l.available_pt)).collect();
    assert_eq!(got[..2], [(10.0, 36.0, 84.0), (22.0, 6.0, 114.0)]);
    // The first baseline's gap is [0, 12]: the 12 + 6 pt indents leave nothing, so the first line
    // (first-line indent included) sets on the second baseline.
    let right_block = [FrameObstacles { frame_id: id("SLYR", 1), rects: vec![Rect { x0_pt: 12.0, y0_pt: 0.0, x1_pt: 120.0, y1_pt: 12.0 }] }];
    let r = styles(&[indents(12.0, 0.0, 6.0)]);
    let p = run_with(frame(120.0, 100.0), &s, &r, Composer::FirstFit, &flow(), &right_block).unwrap();
    let got: Vec<(f64, f64, f64)> = lines(&p).iter().map(|l| (l.baseline_y_pt, l.x_start_pt, l.available_pt)).collect();
    assert_eq!(got, vec![(22.0, 18.0, 102.0), (34.0, 12.0, 108.0)]);
}

/// An indent wider than the frame leaves every slot without width: the paragraph is overset from
/// its start on every composer (no line, no abort, no hang).
#[test]
fn an_indent_wider_than_the_frame_oversets_on_every_composer() {
    // Style 1 carries the settings and sets the second paragraph: the overset one.
    let s = story(&[("aaaa bbbb\n", Some(2)), ("cccc", Some(1))]);
    for settings in [indents(1000.0, 0.0, 0.0), indents(0.0, 1000.0, 0.0), indents(0.0, 0.0, 120.0)] {
        let r = styles(&[settings, StudioCompositionSettings::default()]);
        for composer in composers() {
            let p = run(frame(120.0, 100.0), &s, &r, composer).unwrap();
            assert_eq!(lines(&p).len(), 1, "{composer:?}: only the unindented first paragraph sets");
            assert_eq!(p.overset, Some(SourceSpan { start: 10, end: 14 }), "{composer:?}");
        }
    }
}

/// The last-line indent narrows the last line from the right. When the last line no longer fits,
/// the paragraph breaks once more with that line narrowed; the new last line is narrowed at the set
/// stage, and a last line that still does not fit is overfull. A negative value widens the last line
/// at the set stage only. Right-aligned, the last line ends at the indent.
#[test]
fn last_line_indent_narrows_the_last_line_with_one_rebreak() {
    let last = |v: f64| StudioCompositionSettings { last_line_indent: pt(v), ..Default::default() };
    let s = one("aaaa bbbb cc dd");
    let p = run(frame(60.0, 100.0), &s, &styles(&[last(36.0)]), Composer::FirstFit).unwrap();
    let got: Vec<(u64, f64, bool)> = lines(&p).iter().map(|l| (l.source.start, l.available_pt, l.overfull)).collect();
    assert_eq!(got, vec![(0, 60.0, false), (10, 60.0, false), (13, 24.0, false)], "cc dd (30) does not fit 24: re-broken");
    let plain = run(frame(60.0, 100.0), &s, &styles(&[last(-10.0)]), Composer::FirstFit).unwrap();
    let got: Vec<(u64, f64)> = lines(&plain).iter().map(|l| (l.source.start, l.available_pt)).collect();
    assert_eq!(got, vec![(0, 60.0), (10, 70.0)], "negative: breaks unchanged, the last line widened");
    let word = run(frame(60.0, 100.0), &one("aaaa bbbb cccc"), &styles(&[last(40.0)]), Composer::FirstFit).unwrap();
    let l = lines(&word);
    assert_eq!((l.len(), l[1].available_pt, l[1].overfull), (2, 20.0, true), "one word wider than the narrowed line");
    let right = styles_aligned(StudioTextAlign::Right, &[last(36.0)]);
    let p = run(frame(60.0, 100.0), &s, &right, Composer::FirstFit).unwrap();
    let l = lines(&p);
    assert_eq!((l[2].x_start_pt, l[2].x_start_pt + l[2].width_pt), (12.0, 24.0));
    for composer in composers() {
        let p = run(frame(60.0, 100.0), &s, &styles(&[last(36.0)]), composer).unwrap();
        let l = lines(&p);
        let end = l.last().unwrap();
        assert_eq!(end.available_pt, 24.0, "{composer:?}");
        assert!(end.width_pt <= 24.0 + 1e-9 || end.overfull, "{composer:?}: {end:?}");
        assert!(l[..l.len() - 1].iter().all(|x| x.available_pt == 60.0), "{composer:?}");
    }
}

/// Space after a paragraph and space before the next add as one gap; the story's first paragraph
/// takes none; negative space moves the line up; a paragraph at a column top takes none (a gap that
/// pushes past the column bottom lands the paragraph at the next column's top).
#[test]
fn space_before_and_after_add_one_gap_dropped_at_a_column_top() {
    let s = story(&[("aaaa\n", Some(1)), ("bbbb\n", Some(1)), ("cccc", Some(2))]);
    let r = styles(&[space(6.0, 4.0), space(-6.0, 0.0)]);
    let p = run(frame(120.0, 200.0), &s, &r, Composer::FirstFit).unwrap();
    let got: Vec<f64> = lines(&p).iter().map(|l| l.baseline_y_pt).collect();
    assert_eq!(got, vec![10.0, 32.0, 42.0], "10; 10 + 12 + 4 + 6; 32 + 12 + 4 - 6");
    // Two 60 pt columns two lines deep: 20 pt before pushes "bbbb" past the bottom of column 0.
    let s = story(&[("aaaa\n", Some(1)), ("bbbb", Some(2))]);
    let r = styles(&[StudioCompositionSettings::default(), space(20.0, 0.0)]);
    let p = run(columns(130.0, 24.0, 2, 10.0), &s, &r, Composer::FirstFit).unwrap();
    let got: Vec<(usize, f64, f64)> = lines(&p).iter().map(|l| (l.column, l.baseline_y_pt, l.x_start_pt)).collect();
    assert_eq!(got, vec![(0, 10.0, 0.0), (1, 10.0, 70.0)]);
    let first = run(frame(120.0, 200.0), &one("aaaa"), &styles(&[space(20.0, 0.0)]), Composer::FirstFit).unwrap();
    assert_eq!(lines(&first)[0].baseline_y_pt, 10.0, "the story's first paragraph takes no space before");
}

/// `space_between_same_style`: a paragraph whose effective flag is true and that shares its
/// paragraph style with the paragraph before it is set with no gap; another style keeps the gap, as
/// does an unstyled pair (no shared style), and a local override can set the flag.
#[test]
fn space_between_same_style_suppresses_the_gap() {
    let same = StudioCompositionSettings { space_between_same_style: Some(true), ..space(6.0, 4.0) };
    let r = styles(&[same, space(6.0, 4.0)]);
    let s = story(&[("aa\n", Some(1)), ("bb\n", Some(1)), ("cc\n", Some(2)), ("dd", Some(2))]);
    let got: Vec<f64> = lines(&run(frame(120.0, 300.0), &s, &r, Composer::FirstFit).unwrap()).iter().map(|l| l.baseline_y_pt).collect();
    assert_eq!(got, vec![10.0, 22.0, 44.0, 66.0]);
    let mut local = r.clone();
    let flag = StudioCompositionSettings { space_between_same_style: Some(true), ..Default::default() };
    local.paragraph_composition.push(StudioParagraphComposition { text_id: id("STXT", 1), paragraph_start: 9, settings: flag.clone() });
    let got: Vec<f64> = lines(&run(frame(120.0, 300.0), &s, &local, Composer::FirstFit).unwrap()).iter().map(|l| l.baseline_y_pt).collect();
    assert_eq!(got, vec![10.0, 22.0, 44.0, 56.0], "the override sets the flag on the last paragraph");
    let unstyled = story(&[("aa\n", None), ("bb", None)]);
    let mut r = styles(&[]);
    for start in [0, 3] {
        r.paragraph_composition.push(StudioParagraphComposition { text_id: id("STXT", 1), paragraph_start: start, settings: StudioCompositionSettings { space_before: pt(6.0), ..flag.clone() } });
    }
    let got: Vec<f64> = lines(&run(frame(120.0, 300.0), &unstyled, &r, Composer::FirstFit).unwrap()).iter().map(|l| l.baseline_y_pt).collect();
    assert_eq!(got, vec![10.0, 28.0], "no shared style: the gap stays");
}

fn pflow(start: u64, kind: StudioSpanColumn, before: f64, after: f64) -> StudioParagraphFlow {
    StudioParagraphFlow {
        story_id: id("STXT", 1),
        paragraph_start: start,
        span_column: kind,
        span_count: Some(StudioSpanCount::All),
        span_column_count: 1,
        split_column_count: 2,
        space_before: len(before),
        space_after: len(after),
        inside_gutter: len(0.0),
        outside_gutter: len(0.0),
    }
}

/// Span and split paragraphs take the larger of the flow record's space and the attribute space
/// before and after them; a split paragraph's indents narrow each sub-column.
#[test]
fn span_and_split_take_the_larger_of_flow_and_attribute_space() {
    let s = story(&[("aaaa\n", None), ("bbbb\n", Some(1)), ("cccc", None)]);
    let mut fl = flow();
    fl.paragraph_flows.push(pflow(5, StudioSpanColumn::SpanColumns, 5.0, 3.0));
    let r = styles(&[space(9.0, 8.0)]);
    let p = run_with(columns(130.0, 200.0, 2, 10.0), &s, &r, Composer::FirstFit, &fl, &[]).unwrap();
    let got: Vec<f64> = lines(&p).iter().map(|l| l.baseline_y_pt).collect();
    // Band bottom 12; span top 12 + max(5, 9) = 21, baseline 31; next band top 31 + 2 + max(3, 8) = 41.
    assert_eq!(got, vec![10.0, 31.0, 51.0]);
    let r = styles(&[space(1.0, 1.0)]);
    let p = run_with(columns(130.0, 200.0, 2, 10.0), &s, &r, Composer::FirstFit, &fl, &[]).unwrap();
    assert_eq!(lines(&p).iter().map(|l| l.baseline_y_pt).collect::<Vec<_>>(), vec![10.0, 27.0, 42.0], "the flow values win");
    // Split in two 30 pt sub-columns of column 0, 3 pt left indent: x 3, measure 27.
    let mut fl = flow();
    fl.paragraph_flows.push(pflow(5, StudioSpanColumn::SplitColumns, 3.0, 2.0));
    let r = styles(&[StudioCompositionSettings { left_indent: pt(3.0), ..space(7.0, 5.0) }]);
    let p = run_with(columns(130.0, 200.0, 2, 10.0), &s, &r, Composer::FirstFit, &fl, &[]).unwrap();
    let got: Vec<(f64, f64, f64)> = lines(&p).iter().map(|l| (l.baseline_y_pt, l.x_start_pt, l.available_pt)).collect();
    assert_eq!(got, vec![(10.0, 0.0, 60.0), (29.0, 3.0, 27.0), (46.0, 0.0, 60.0)], "10 + 12 + max(3, 7); 29 + 12 + max(2, 5)");
}

/// A break sentinel moves the flow to its target and drops the pending space after.
#[test]
fn a_break_sentinel_drops_the_pending_space() {
    let s = story(&[(&format!("aaaa{COLUMN_BREAK}"), Some(1)), ("bbbb", Some(1))]);
    let r = styles(&[space(0.0, 30.0)]);
    let p = run(columns(130.0, 200.0, 2, 10.0), &s, &r, Composer::FirstFit).unwrap();
    let got: Vec<(usize, f64)> = lines(&p).iter().map(|l| (l.column, l.baseline_y_pt)).collect();
    assert_eq!(got, vec![(0, 10.0), (1, 10.0)]);
}

/// Column balancing keeps the paragraph space between lines of one column and drops it at a
/// column's first line (it used to rebuild every baseline without it).
#[test]
fn balancing_keeps_paragraph_space() {
    let s = story(&[("aa\n", Some(1)), ("bb\n", Some(1)), ("cc\n", Some(1)), ("dd", Some(1))]);
    let r = styles(&[space(6.0, 0.0)]);
    let f = columns(130.0, 70.0, 2, 10.0);
    let ragged = run(f, &s, &r, Composer::FirstFit).unwrap();
    assert_eq!(lines(&ragged).iter().map(|l| (l.column, l.baseline_y_pt)).collect::<Vec<_>>(), vec![(0, 10.0), (0, 28.0), (0, 46.0), (0, 64.0)]);
    let mut fl = flow();
    fl.frame_options.push(StudioFrameFlowOptions { layer_id: id("SLYR", 1), vertical_balance_columns: true });
    let balanced = run_with(f, &s, &r, Composer::FirstFit, &fl, &[]).unwrap();
    assert_eq!(lines(&balanced).iter().map(|l| (l.column, l.baseline_y_pt)).collect::<Vec<_>>(), vec![(0, 10.0), (0, 28.0), (1, 10.0), (1, 28.0)]);
}

/// Width auto-size counts each line's indents (left, first line, right and a positive last-line
/// indent): the final composition at the found width does not wrap.
#[test]
fn width_autosize_counts_the_indents() {
    let r = styles(&[StudioCompositionSettings { last_line_indent: pt(7.0), ..indents(10.0, 5.0, 0.0) }]);
    let s = one("aaaa");
    let doc = document(F { auto: AutoSizing::WidthOnly, ..frame(200.0, 100.0) });
    let a = reflow_autosize(&request(&doc, &s, &[], Composer::FirstFit), &opts(&r), &flow(), &CancellationToken::default()).unwrap();
    assert_eq!(a.resizes[0].after_width_pt, 24.0 + 10.0 + 5.0 + 7.0);
    let l = &a.layout.frames[0].lines[0];
    assert_eq!((l.x_start_pt, l.available_pt, l.overfull), (10.0, 24.0, false));
}

/// Hostile values (struct literals; Folio refuses non-finite and non-point values on the wire): the
/// largest finite values give a finite placement or `invalid_geometry`, never a NaN slot, an abort or
/// a hang; a sum that overflows is `invalid_geometry`.
#[test]
fn hostile_indent_and_space_values_give_typed_answers() {
    let max = f64::MAX;
    let s = story(&[("aaaa bbbb\n", Some(1)), ("cccc\n", Some(1)), ("dddd", Some(1))]);
    let each = [
        indents(max, 0.0, 0.0),
        indents(0.0, max, 0.0),
        indents(0.0, 0.0, max),
        indents(-max, 0.0, 0.0),
        StudioCompositionSettings { last_line_indent: pt(max), ..Default::default() },
        StudioCompositionSettings { last_line_indent: pt(-max), ..Default::default() },
        space(max, 0.0),
        space(0.0, max),
        space(-max, 0.0),
        space(0.0, -max),
    ];
    for settings in each {
        for composer in composers() {
            match run(frame(120.0, 100.0), &s, &styles(std::slice::from_ref(&settings)), composer) {
                Ok(p) => assert!(finite(&p), "{settings:?} {composer:?}: {p:?}"),
                Err(e) => assert_eq!(e, LayoutError::InvalidGeometry, "{settings:?} {composer:?}"),
            }
        }
    }
    // Sums past f64::MAX: both sides of the measure, the first line on the left indent, and space
    // accumulated in one column.
    for settings in [indents(-max, -max, 0.0), indents(-max, 0.0, -max), space(-max, 0.0)] {
        for composer in composers() {
            assert_eq!(run(frame(120.0, 100.0), &s, &styles(std::slice::from_ref(&settings)), composer), Err(LayoutError::InvalidGeometry), "{settings:?} {composer:?}");
        }
    }
    // Alternating +MAX / -MAX space: each gap is finite (MAX - MAX) or a push off the frame.
    let alt = story(&[("aa\n", Some(1)), ("bb\n", Some(2)), ("cc\n", Some(1)), ("dd", Some(2))]);
    let r = styles(&[space(max, -max), space(-max, max)]);
    for composer in composers() {
        match run(frame(120.0, 100.0), &alt, &r, composer) {
            Ok(p) => assert!(finite(&p), "{composer:?}: {p:?}"),
            Err(e) => assert_eq!(e, LayoutError::InvalidGeometry, "{composer:?}"),
        }
    }
}

/// `paragraph_params` resolves the indents and spacing to points (unset = 0, any unit through
/// `length_pt`; a non-finite value is `invalid_geometry`) and settings holding only them leave the
/// composer, H&J limits and hyphenation exactly the request's.
#[test]
fn paragraph_params_maps_indents_and_leaves_the_composer_alone() {
    let hy = HyphenationParams { hyphen_advance_pt: 6.0, min_word_len: usize::MAX, ..HyphenationParams::default() };
    let o = TypesetOptions { hyphenation: Some(hy), ..Default::default() };
    let geometry = StudioCompositionSettings {
        left_indent: Some(Length { value: 10.0, unit: GeometryUnit::Mm }),
        right_indent: pt(-2.0),
        first_line_indent: pt(3.0),
        last_line_indent: pt(4.0),
        space_before: pt(5.0),
        space_after: pt(6.0),
        space_between_same_style: Some(true),
        ..Default::default()
    };
    let c = ParagraphComposition { settings: geometry.clone(), space_advance_pt: Some(3.0), hyphen_advance_pt: None };
    let kp = Composer::KnuthPlass(KnuthPlassParams::default());
    let base = paragraph_params(None, 12.0, kp, SpaceParams::default(), &o).unwrap();
    let p = paragraph_params(Some(&c), 12.0, kp, SpaceParams::default(), &o).unwrap();
    assert_eq!(
        p.geometry,
        ParagraphGeometry { left_indent_pt: 10.0 * 72.0 / 25.4, right_indent_pt: -2.0, first_line_indent_pt: 3.0, last_line_indent_pt: 4.0, space_before_pt: 5.0, space_after_pt: 6.0 }
    );
    assert_eq!(ParagraphParams { geometry: ParagraphGeometry::default(), ..p }, base, "nothing but the geometry differs");
    assert_eq!(base.geometry, ParagraphGeometry::default(), "the engine default is 0");
    // With a composer setting beside them the composer setting applies as before.
    let scope = StudioCompositionSettings { composition_scope: Some(StudioCompositionScope::SingleLine), ..Default::default() };
    let alone = paragraph_params(Some(&ParagraphComposition { settings: scope.clone(), ..c.clone() }), 12.0, kp, SpaceParams::default(), &o).unwrap();
    let both = paragraph_params(Some(&ParagraphComposition { settings: scope.overlaid(&geometry), ..c.clone() }), 12.0, kp, SpaceParams::default(), &o).unwrap();
    assert_eq!(ParagraphParams { geometry: ParagraphGeometry::default(), ..both }, alone);
    assert_eq!(both.geometry, p.geometry);
    let nan = ParagraphComposition { settings: StudioCompositionSettings { space_after: pt(f64::NAN), ..Default::default() }, ..c };
    assert_eq!(paragraph_params(Some(&nan), 12.0, kp, SpaceParams::default(), &o), Err(LayoutError::InvalidGeometry));
}

/// `shift_paragraph_records` (V4-06 class C): the paragraph-keyed records of the edited story move
/// with their paragraphs; a record naming no paragraph after the edit is dropped, and of two that
/// collapse onto one start the one of the first touched paragraph (the lower original start) is
/// kept; another story's records stay.
#[test]
fn shift_paragraph_records_moves_drops_and_keeps_the_first() {
    let s = story(&[("aa\n", None), ("bb\n", None), ("cc", None)]).story;
    let entry = |text: u32, start: u64, v: f64| StudioParagraphComposition { text_id: id("STXT", text), paragraph_start: start, settings: indents(v, 0.0, 0.0) };
    let mut styles = StudioTypeStyleRecords::new(id("SDOC", 1));
    styles.paragraph_composition = vec![entry(1, 6, 3.0), entry(1, 3, 2.0), entry(1, 0, 1.0), entry(2, 6, 9.0)];
    let mut fl = flow();
    fl.paragraph_flows = vec![pflow(6, StudioSpanColumn::SingleColumn, 0.0, 0.0), pflow(3, StudioSpanColumn::SingleColumn, 0.0, 0.0)];
    let mine = |r: &StudioTypeStyleRecords| r.paragraph_composition.iter().map(|c| (c.text_id == id("STXT", 1), c.paragraph_start, c.settings.left_indent.as_ref().map(|l| l.value))).collect::<Vec<_>>();
    // Typing inside paragraph 0 moves every later start of the story by one byte.
    let (mut st, mut f) = (styles.clone(), fl.clone());
    let edited = replace_story_text(&s, None, 1, 1, "x").unwrap();
    shift_paragraph_records(&mut f, &mut st, &edited, 1, 1, 1).unwrap();
    assert_eq!(mine(&st), vec![(true, 7, Some(3.0)), (true, 4, Some(2.0)), (true, 0, Some(1.0)), (false, 6, Some(9.0))]);
    assert_eq!(f.paragraph_flows.iter().map(|p| p.paragraph_start).collect::<Vec<_>>(), vec![7, 4]);
    // Deleting "aa\n" collapses paragraphs 0 and 1 onto start 0: paragraph 0's override is kept.
    let (mut st, mut f) = (styles.clone(), fl.clone());
    let edited = replace_story_text(&s, None, 0, 3, "").unwrap();
    assert_eq!(edited.paragraphs.iter().map(|p| p.start).collect::<Vec<_>>(), vec![0, 3]);
    shift_paragraph_records(&mut f, &mut st, &edited, 0, 3, 0).unwrap();
    assert_eq!(mine(&st), vec![(true, 3, Some(3.0)), (true, 0, Some(1.0)), (false, 6, Some(9.0))]);
    assert_eq!(f.paragraph_flows.iter().map(|p| p.paragraph_start).collect::<Vec<_>>(), vec![3, 0]);
    // Deleting the terminator "\n" of paragraph 0 merges it with paragraph 1, whose records go.
    let (mut st, mut f) = (styles, fl);
    let edited = replace_story_text(&s, None, 2, 3, "").unwrap();
    assert_eq!(edited.paragraphs.iter().map(|p| p.start).collect::<Vec<_>>(), vec![0, 5]);
    shift_paragraph_records(&mut f, &mut st, &edited, 2, 3, 0).unwrap();
    assert_eq!(mine(&st), vec![(true, 5, Some(3.0)), (true, 0, Some(1.0)), (false, 6, Some(9.0))]);
    assert_eq!(f.paragraph_flows.iter().map(|p| p.paragraph_start).collect::<Vec<_>>(), vec![5]);
}

/// Item 3 measurement (not a proof; ignored, run on demand in debug and release): one paragraph of
/// 600, 1800 and 5000 words per composer, with no, a 30 pt and a 170 pt last-line indent (170 on a
/// 180 pt measure re-breaks every time), prints `MEASURE:` lines (outcome, lines, time). The
/// refusal point is the composer's own budget (`budget_exceeded`).
#[test]
#[ignore = "measurement of the last-line re-break cost; run with --run-ignored only"]
fn measure_last_line_indent_rebreak_cost() {
    let word = ["aaaa", "bb", "cccccc", "ddd", "eeeee"];
    for words in [600usize, 1800, 5000] {
        let text: String = (0..words).map(|i| word[i % word.len()]).collect::<Vec<_>>().join(" ");
        let s = one(&text);
        for composer in composers() {
            for last in [0.0, 30.0, 170.0] {
                // Debug builds skip the 5000-word worst case (Knuth-Plass takes minutes there).
                if last > 100.0 && words > 1800 && cfg!(debug_assertions) {
                    continue;
                }
                let r = styles(&[StudioCompositionSettings { last_line_indent: pt(last), ..Default::default() }]);
                let t = std::time::Instant::now();
                let out = run(frame(180.0, 1_000_000.0), &s, &r, composer);
                let ms = t.elapsed().as_secs_f64() * 1000.0;
                let what = match &out {
                    Ok(p) => format!("ok lines={} overset={}", lines(p).len(), p.overset.is_some()),
                    Err(e) => e.code().to_string(),
                };
                println!("MEASURE: words={words} composer={composer:?} last_line_indent={last} {what} ms={ms:.1}");
            }
        }
    }
}

/// Lead ruling 2026-11-08 (item 3 stop rule): a last line narrowed below its widest word is not
/// re-broken. The breaks of the paragraph stand and the last line is overfull, a typed outcome
/// (`Line::overfull`), never a clip; a normal indent still re-breaks once. Measured worst case:
/// 170 pt on a 180 pt measure (10 pt left, narrower than every word), 600 words on every composer
/// and 5000 words on the Adobe paragraph composer (it refused `budget_exceeded` there before).
#[test]
fn a_last_line_narrower_than_its_widest_word_keeps_the_breaks_and_is_overfull() {
    let words = ["aaaa", "bb", "cccccc", "ddd", "eeeee"];
    let text = |n: usize| (0..n).map(|i| words[i % words.len()]).collect::<Vec<_>>().join(" ");
    let last = |v: f64| styles(&[StudioCompositionSettings { last_line_indent: pt(v), ..Default::default() }]);
    let starts = |p: &LayoutProposal| lines(p).iter().map(|l| l.source.start).collect::<Vec<_>>();
    let cases: Vec<(usize, Composer)> =
        composers().into_iter().map(|c| (600, c)).chain([(5000, Composer::AdobeParagraph(AdobeParams::default()))]).collect();
    for (n, composer) in cases {
        let s = one(&text(n));
        let base = run(frame(180.0, 1_000_000.0), &s, &last(0.0), composer).unwrap();
        let narrow = run(frame(180.0, 1_000_000.0), &s, &last(170.0), composer).unwrap();
        assert_eq!(starts(&narrow), starts(&base), "{n} words {composer:?}: breaks unchanged");
        let (b, l) = (lines(&base), lines(&narrow));
        let end = l.last().unwrap();
        assert!(end.overfull && end.available_pt == 10.0, "{composer:?}: {end:?}");
        assert!(end.justification.is_none(), "an overfull line is not justified");
        assert_eq!(l[..l.len() - 1], b[..b.len() - 1], "{composer:?}: the other lines are untouched");
    }
}

/// Lead ruling 2026-11-08 (item 5): text inserted at byte 0 of a non-empty story belongs to the
/// first paragraph (the paragraph partition still starts at 0).
#[test]
fn text_inserted_at_byte_zero_joins_the_first_paragraph() {
    let s = story(&[("aa\n", None), ("bb", None)]).story;
    let edited = replace_story_text(&s, None, 0, 0, "xy").unwrap();
    assert_eq!(edited.paragraphs.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>(), vec![(0, 5), (5, 7)]);
    let empty = story(&[("", None)]).story;
    let e = replace_story_text(&empty, None, 0, 0, "q").unwrap();
    assert_eq!(e.paragraphs.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>(), vec![(0, 1)]);
}

fn override_at(start: u64) -> StudioTypeStyleRecords {
    let mut r = StudioTypeStyleRecords::new(id("SDOC", 1));
    r.paragraph_composition.push(StudioParagraphComposition { text_id: id("STXT", 1), paragraph_start: start, settings: indents(9.0, 0.0, 0.0) });
    r
}

/// V4-06 class C, `write_cross_reference_with_styles`: writing a reference before an overridden
/// paragraph moves the override and the paragraph's flow record with it.
#[test]
fn a_cross_reference_write_moves_the_paragraph_keyed_records() {
    let s = story(&[("X\n", None), ("bbbb", None)]).story;
    let mut fl = flow();
    fl.paragraph_flows.push(pflow(2, StudioSpanColumn::SingleColumn, 0.0, 0.0));
    fl.cross_references.push(StudioCrossReference {
        xref_id: "x1".into(),
        source_story_id: id("STXT", 1),
        source_start: 0,
        source_end: 1,
        destination: StudioXrefDestination::Paragraph { story_id: id("STXT", 1), paragraph_start: 2 },
        format_id: "f".into(),
        resolved_text: None,
    });
    let mut st = override_at(2);
    let written = write_cross_reference_with_styles(&mut fl, &mut st, &s, None, "x1", "Hello").unwrap();
    assert_eq!(written.paragraphs.iter().map(|p| p.start).collect::<Vec<_>>(), vec![0, 6]);
    assert_eq!((st.paragraph_composition[0].paragraph_start, fl.paragraph_flows[0].paragraph_start), (6, 6));
}

/// V4-06 class C, `apply_text_changes_with_records`: a change before an overridden paragraph moves
/// its records; the undo keeps the records as they were.
#[test]
fn find_change_moves_the_paragraph_keyed_records_and_undo_keeps_them() {
    let s = story(&[("aaaa bbbb\n", None), ("cccc", None)]).story;
    let mut fl = flow();
    fl.paragraph_flows.push(pflow(10, StudioSpanColumn::SingleColumn, 0.0, 0.0));
    let mut st = override_at(10);
    let (fl0, st0) = (fl.clone(), st.clone());
    let hits = [FindChangeHit { story_id: id("STXT", 1), start: 0, end: 4, replacement: Some("x".into()) }];
    let (changed, undo) = apply_text_changes_with_records(std::slice::from_ref(&s), &hits, &mut fl, &mut st).unwrap();
    assert_eq!(changed[0].paragraphs.iter().map(|p| p.start).collect::<Vec<_>>(), vec![0, 7]);
    assert_eq!((st.paragraph_composition[0].paragraph_start, fl.paragraph_flows[0].paragraph_start), (7, 7));
    assert_eq!((undo.flow_before, undo.styles_before), (Some(fl0), Some(st0)));
}

/// Review F1: text typed or pasted at the start of paragraph P belongs to P (P keeps its style), for
/// P = 0 and P = 2; text typed inside a paragraph stays that paragraph's; a merge keeps the first
/// touched paragraph's style.
#[test]
fn text_at_a_paragraph_start_belongs_to_that_paragraph() {
    let s = story(&[("aa\n", Some(1)), ("bb\n", Some(2)), ("cc", Some(3))]).story;
    let styled = |e: StudioTextStory| e.paragraphs.iter().map(|p| (p.start, p.style_ref.clone())).collect::<Vec<_>>();
    let st = |n: u32| Some(id("STYS", n));
    let edit = |a: u64, b: u64, t: &str| styled(replace_story_text(&s, None, a, b, t).unwrap());
    assert_eq!(edit(0, 0, "x"), vec![(0, st(1)), (4, st(2)), (7, st(3))], "typed at P = 0");
    assert_eq!(edit(6, 6, "x"), vec![(0, st(1)), (3, st(2)), (6, st(3))], "typed at P = 2");
    assert_eq!(edit(0, 0, "xy\nz"), vec![(0, st(1)), (3, st(1)), (7, st(2)), (10, st(3))], "pasted at P = 0");
    assert_eq!(edit(6, 6, "xy\nz"), vec![(0, st(1)), (3, st(2)), (6, st(3)), (9, st(3))], "pasted at P = 2");
    assert_eq!(edit(4, 4, "\n"), vec![(0, st(1)), (3, st(2)), (5, st(2)), (7, st(3))], "a split inside P = 1");
    assert_eq!(edit(2, 3, ""), vec![(0, st(1)), (5, st(3))], "a merge: the first touched wins");
}

/// Review F2: a paragraph whose first line opened a column (its space dropped there) gets its
/// space back when balancing moves that line to the middle of a column.
#[test]
fn balancing_gives_back_the_space_a_column_top_dropped() {
    // Two 60 pt columns, three lines deep; A fills column 0, B (space before 6) opens column 1.
    let s = story(&[("aaaaaaaa bbbbbbbb cccccccc\n", Some(1)), ("dd", Some(2))]);
    let r = styles(&[StudioCompositionSettings::default(), space(6.0, 0.0)]);
    let f = columns(130.0, 36.0, 2, 10.0);
    let ragged = run(f, &s, &r, Composer::FirstFit).unwrap();
    assert_eq!(lines(&ragged).iter().map(|l| (l.column, l.baseline_y_pt)).collect::<Vec<_>>(), vec![(0, 10.0), (0, 22.0), (0, 34.0), (1, 10.0)]);
    let mut fl = flow();
    fl.frame_options.push(StudioFrameFlowOptions { layer_id: id("SLYR", 1), vertical_balance_columns: true });
    let balanced = run_with(f, &s, &r, Composer::FirstFit, &fl, &[]).unwrap();
    assert_eq!(
        lines(&balanced).iter().map(|l| (l.column, l.baseline_y_pt)).collect::<Vec<_>>(),
        vec![(0, 10.0), (0, 22.0), (1, 10.0), (1, 28.0)],
        "B sits at row 1 of column 1: 10 + 12 + 6"
    );
}

/// Review F3: on both Adobe composers a last line its spaces can compress into the narrowed width
/// fits: no re-break, not overfull, its spaces closed to the measure (as the set stage does).
#[test]
fn adobe_last_line_fit_counts_its_compressible_spaces() {
    // "aaaaaaaaaa" / "aa bb cc" (48 pt, two 6 pt spaces shrinking 20 %: 45.6 pt) in 60 pt; the
    // last line narrowed by 13 pt to 47 pt.
    let s = one("aaaaaaaaaa aa bb cc");
    let r = styles(&[StudioCompositionSettings { last_line_indent: pt(13.0), ..Default::default() }]);
    for composer in [Composer::AdobeSingleLine(AdobeParams::default()), Composer::AdobeParagraph(AdobeParams::default())] {
        let p = run(frame(60.0, 100.0), &s, &r, composer).unwrap();
        let l = lines(&p);
        assert_eq!(l.iter().map(|x| x.source.start).collect::<Vec<_>>(), vec![0, 11], "{composer:?}");
        let end = &l[1];
        assert!(!end.overfull && end.available_pt == 47.0, "{composer:?}: {end:?}");
        let j = end.justification.expect("compressed");
        assert!(j.word_scale < 1.0 && j.word_scale >= 0.8, "{composer:?}: {j:?}");
    }
}

/// Review F4: split paragraphs get no last-line re-break (their breaks are taken for the
/// sub-column grid); a narrowed last line that does not fit is overfull, whatever the gate says.
#[test]
fn split_paragraphs_take_no_last_line_rebreak() {
    let s = story(&[("aaaa bb cc", Some(1))]);
    let mut fl = flow();
    fl.paragraph_flows.push(pflow(0, StudioSpanColumn::SplitColumns, 0.0, 0.0));
    // Two 30 pt sub-columns: "aaaa " / "bb cc" (30 pt); the last line narrowed by 12 to 18 pt holds
    // every word (12 pt) but not the line.
    let r = styles(&[StudioCompositionSettings { last_line_indent: pt(12.0), ..Default::default() }]);
    let p = run_with(columns(130.0, 200.0, 2, 10.0), &s, &r, Composer::FirstFit, &fl, &[]).unwrap();
    let l = lines(&p);
    assert_eq!(l.iter().map(|x| x.source.start).collect::<Vec<_>>(), vec![0, 5]);
    assert!(l[1].overfull && l[1].available_pt == 18.0, "{:?}", l[1]);
}
