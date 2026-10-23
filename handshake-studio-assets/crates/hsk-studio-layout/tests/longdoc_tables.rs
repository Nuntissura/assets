//! SMB-LAYOUT-LONGDOC slice 1: tables. Hand-computed oracles. Toy font: every glyph advances 6 pt,
//! line height 12 pt, ascent 10 pt (descent 2 pt). Frames have no insets; cells have no insets, so a
//! cell's first baseline is 10 pt below its top and a one-line cell is 12 pt tall.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
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
fn page(key: &str) -> StudioPage {
    StudioPage { page_id: key.into(), width: len(100.0), height: len(100.0), side: PageSide::RightHand, margins: edges(5.0), bleed: None }
}
/// Three frames of story 1 (layers 1..=3), 60 pt wide and `h` tall: two on page-1, one on page-2.
fn document(h: f64) -> StudioDocument {
    let frame = |n: u32, page: &str, y: f64| StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", n),
        name: format!("frame {n}"),
        parent: Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: id("SPGS", 1) }),
        kind: "text".into(),
        payload: Payload::Text { story_id: id("STXT", 1) },
        visible: true,
        appearance: None,
        text_frame: Some(StudioTextFrame {
            page_id: page.into(),
            bounds: Bounds { x: len(10.0), y: len(y), width: len(60.0), height: len(h) },
            inset_spacing: edges(0.0),
            text_column_count: 1,
            text_column_gutter: len(0.0),
            first_baseline_offset: FirstBaseline::Fixed,
            minimum_first_baseline_offset: len(10.0),
            vertical_justification: VerticalJustification::Top,
            auto_sizing_type: AutoSizing::Off,
            previous_text_frame: (n > 1).then(|| id("SLYR", n - 1)),
            next_text_frame: (n < 3).then(|| id("SLYR", n + 1)),
        }),
    };
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision: 3,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: ProfileBinding::Unassigned(()),
        working_space_cmyk: ProfileBinding::Unassigned(()),
        working_space_gray: ProfileBinding::Unassigned(()),
        working_space_spot: ProfileBinding::Unassigned(()),
        blending_space: ProfileBinding::Unassigned(()),
        artboards: vec![],
        page_spreads: vec![StudioPageSpread {
            schema_id: PageSpreadSchema::V1,
            page_spread_id: id("SPGS", 1),
            name: "spread".into(),
            parent: Parent::Root(()),
            bounds: Bounds { x: len(0.0), y: len(0.0), width: len(200.0), height: len(100.0) },
            pages: vec![page("page-1"), page("page-2")],
        }],
        layers: vec![frame(1, "page-1", 0.0), frame(2, "page-1", 40.0), frame(3, "page-2", 0.0)],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    }
}
fn story(n: u32, text: &str, paragraphs: &[(u64, u64)]) -> StudioTextStory {
    let end = text.len() as u64;
    StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", n),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.into() },
        paragraphs: paragraphs.iter().map(|&(start, end)| StoryParagraph { start, end, style_ref: None, paragraph_direction: None }).collect(),
        character_ranges: vec![StoryRange { start: 0, end, style_ref: None, applied_language: None }],
    }
}
/// Paragraph ranges split at '\n' (the newline ends its paragraph) and their toy shaping.
fn paragraphs(text: &str) -> (Vec<(u64, u64)>, Vec<ShapedParagraph>) {
    let mut ranges = Vec::new();
    let mut start = 0usize;
    for (i, c) in text.char_indices() {
        if c == '\n' {
            ranges.push((start as u64, (i + 1) as u64));
            start = i + 1;
        }
    }
    if start < text.len() {
        ranges.push((start as u64, text.len() as u64));
    }
    let shaped = ranges
        .iter()
        .map(|&(s, e)| ShapedParagraph::from_fixed_advance(text, SourceSpan { start: s, end: e }, 6.0, 12.0, 10.0).unwrap())
        .collect();
    (ranges, shaped)
}
fn row(row_type: TableRowType, min: f64) -> StudioTableRow {
    StudioTableRow { row_type, minimum_height: len(min), maximum_height: len(500.0), keep_with_next_row: false, start_row: TableStartRow::Anywhere, allow_split: false }
}
fn cell(row: u32, column: u32, content: Option<(u64, u64)>) -> StudioTableCell {
    StudioTableCell {
        row,
        column,
        row_span: 1,
        column_span: 1,
        content: content.map(|(start, end)| StudioByteRange { start, end }),
        insets: edges(0.0),
        fill: None,
        fill_tint: None,
        strokes: StudioCellEdges::default(),
        stroke_priorities: StudioEdgePriorities::default(),
        vertical_justification: VerticalJustification::Top,
    }
}
fn table(columns: &[f64], rows: Vec<StudioTableRow>, cells: Vec<StudioTableCell>) -> StudioTable {
    StudioTable {
        table_id: "t1".into(),
        story_id: id("STXT", 1),
        anchor: 0,
        cell_story_id: Some(id("STXT", 2)),
        column_widths: columns.iter().map(|w| len(*w)).collect(),
        rows,
        cells,
        break_headers: TableHeaderBreak::OncePerTextFrame,
        break_footers: TableHeaderBreak::OncePerTextFrame,
        skip_first_header: false,
        skip_last_footer: false,
        stroke_order: TableStrokeOrder::RowOnTop,
    }
}
fn run(doc: &StudioDocument, t: &StudioTable, text: &str, start: Option<FlowPosition>) -> Result<TableProposal, TableError> {
    let host = story(1, "x", &[(0, 1)]);
    let (ranges, shaped) = paragraphs(text);
    let cells = story(2, text, &ranges);
    layout_table(
        &TableRequest {
            document: doc,
            table: t,
            host_story: &host,
            host_resolved_text: None,
            cell_story: Some(&cells),
            cell_resolved_text: None,
            cell_paragraphs: &shaped,
            start,
            composer: Composer::FirstFit,
            space: SpaceParams::default(),
            expected_revision: doc.revision,
            limits: TableLimits::default(),
        },
        &TypesetOptions::default(),
        &CancellationToken::default(),
    )
}
fn rows_of(p: &TableProposal, part: usize) -> Vec<(usize, f64, f64, PieceKind)> {
    p.parts[part].rows.iter().map(|r| (r.row, r.y_pt, r.height_pt, r.kind)).collect()
}

/// Header row 0 ("H"), body rows 1..=5 ("A".."E", 12 pt each), footer row 6 (empty, min 10 pt), two
/// 30 pt columns, frames 60 pt tall. Frame 1: header 0..12, rows 1-3 at 12/24/36, row 4 would end at
/// 60 + footer 10 > 60, so the footer follows row 3 at 48. Frame 2: header again, rows 4-5, footer
/// at 36. With skip_first_header and once-per-page footers: frame 1 holds rows 1-4 (0..48, 48 + 10
/// footer reserve = 58), no footer (frame 2 is on the same page); frame 2 shows the header, row 5
/// and the footer. Starting in frame 3 only: rows 4 and 5 do not fit and are overset from row 4.
#[test]
fn table_repeats_header_and_footer_across_threaded_frames() {
    let text = "H\nA\nB\nC\nD\nE";
    let mut rows = vec![row(TableRowType::Header, 10.0)];
    rows.extend((0..5).map(|_| row(TableRowType::Body, 10.0)));
    rows.push(row(TableRowType::Footer, 10.0));
    let mut cells = Vec::new();
    for r in 0..7u32 {
        let content = (r < 6).then(|| (u64::from(r) * 2, u64::from(r) * 2 + if r < 5 { 2 } else { 1 }));
        cells.push(cell(r, 0, content));
        cells.push(cell(r, 1, None));
    }
    let doc = document(60.0);
    let t = table(&[30.0, 30.0], rows, cells);
    let p = run(&doc, &t, text, None).unwrap();
    use PieceKind::*;
    assert_eq!(p.parts.len(), 2);
    assert_eq!(rows_of(&p, 0), vec![(0, 0.0, 12.0, Header), (1, 12.0, 12.0, Body), (2, 24.0, 12.0, Body), (3, 36.0, 12.0, Body), (6, 48.0, 10.0, Footer)]);
    assert_eq!(rows_of(&p, 1), vec![(0, 0.0, 12.0, Header), (4, 12.0, 12.0, Body), (5, 24.0, 12.0, Body), (6, 36.0, 10.0, Footer)]);
    assert_eq!((p.parts[0].frame_id.as_str(), p.parts[1].frame_id.as_str()), (id("SLYR", 1).as_str(), id("SLYR", 2).as_str()));
    let baselines: Vec<(f64, f64)> = p.text.frames[0].lines.iter().map(|l| (l.x_start_pt, l.baseline_y_pt)).collect();
    assert_eq!(baselines, vec![(0.0, 10.0), (0.0, 22.0), (0.0, 34.0), (0.0, 46.0)]);
    assert_eq!(p.text.frames[1].lines.len(), 3, "header text repeats in frame 2");
    assert_eq!(p.overset_row, None);
    assert_eq!(p.end, Some(FlowPosition { frame: 1, y_pt: 46.0 }));

    let mut t2 = t.clone();
    t2.skip_first_header = true;
    t2.break_footers = TableHeaderBreak::OncePerPage;
    let p = run(&doc, &t2, text, None).unwrap();
    assert_eq!(rows_of(&p, 0), vec![(1, 0.0, 12.0, Body), (2, 12.0, 12.0, Body), (3, 24.0, 12.0, Body), (4, 36.0, 12.0, Body)]);
    assert_eq!(rows_of(&p, 1), vec![(0, 0.0, 12.0, Header), (5, 12.0, 12.0, Body), (6, 24.0, 10.0, Footer)]);

    let p = run(&doc, &t, text, Some(FlowPosition { frame: 2, y_pt: 0.0 })).unwrap();
    assert_eq!(p.parts.len(), 1);
    assert_eq!(p.parts[0].frame_id, id("SLYR", 3));
    assert_eq!(p.overset_row, Some(4));
}

/// Three 20 pt columns. Row 0: cell (0,0) spans rows 0-1 and holds "aa bb cc" (3 lines of at most 3
/// glyphs: 36 pt), cell (0,1) spans columns 1-2. Row 1: cells (1,1), (1,2). Row 2: one cell spanning
/// all columns with six 10-glyph lines (72 pt), allow_split. Rows 0/1 are 10 pt minimum, so row 1
/// grows to 26 (36 - 10). Strokes: (0,1) bottom red 1 pt priority 0, (1,1) top blue 2 pt priority 5,
/// (1,2) no top: the boundary at y 10 draws blue over x 20..40 and red over 40..60. Frame 1 (60 pt)
/// holds rows 0-1 (36 pt); row 2 (72 pt) does not fit the rest and moves on; it does not fit an
/// empty frame either, so it splits: frame 2 takes lines 1-5 (bottoms 12..60, piece 60 pt), frame 3
/// line 6 at baseline 10 (piece 12 pt).
#[test]
fn spans_stroke_priorities_and_split_row() {
    let six = "aaaaaaaaa aaaaaaaaa aaaaaaaaa aaaaaaaaa aaaaaaaaa aaaaaaaaa";
    let text = format!("aa bb cc\n{six}");
    let mut rows = vec![row(TableRowType::Body, 10.0), row(TableRowType::Body, 10.0), row(TableRowType::Body, 10.0)];
    rows[2].allow_split = true;
    let colour = |r, g, b| StudioColor { r, g, b, colour_profile_id: ProfileBinding::Unassigned(()) };
    let stroke = |w: f64, c: StudioColor| StudioCellStroke { weight: len(w), colour: c, tint: 100.0 };
    let mut a = cell(0, 0, Some((0, 9)));
    a.row_span = 2;
    let mut b = cell(0, 1, None);
    b.column_span = 2;
    b.strokes.bottom = Some(stroke(1.0, colour(1.0, 0.0, 0.0)));
    let mut c = cell(1, 1, None);
    c.strokes.top = Some(stroke(2.0, colour(0.0, 0.0, 1.0)));
    c.stroke_priorities.top = 5;
    let d = cell(1, 2, None);
    let mut e = cell(2, 0, Some((9, text.len() as u64)));
    e.column_span = 3;
    let doc = document(60.0);
    let t = table(&[20.0, 20.0, 20.0], rows, vec![a, b, c, d, e]);
    let p = run(&doc, &t, &text, None).unwrap();
    use PieceKind::*;
    assert_eq!(rows_of(&p, 0), vec![(0, 0.0, 10.0, Body), (1, 10.0, 26.0, Body)]);
    assert_eq!(rows_of(&p, 1), vec![(2, 0.0, 60.0, Split { index: 0, last: false })]);
    assert_eq!(rows_of(&p, 2), vec![(2, 0.0, 12.0, Split { index: 1, last: true })]);
    let spanned = p.cells.iter().find(|c| c.cell == 0).unwrap();
    assert_eq!((spanned.rect.y0_pt, spanned.rect.y1_pt), (0.0, 36.0));
    assert_eq!(p.text.frames[0].lines.iter().map(|l| l.baseline_y_pt).collect::<Vec<_>>(), vec![10.0, 22.0, 34.0]);
    assert_eq!(p.text.frames[1].lines.len(), 5);
    assert_eq!(p.text.frames[2].lines.iter().map(|l| l.baseline_y_pt).collect::<Vec<_>>(), vec![10.0]);
    let at_10: Vec<(f64, f64, f64)> = p
        .strokes
        .iter()
        .filter(|s| s.part == 0 && s.horizontal && s.from[1] == 10.0)
        .map(|s| (s.from[0], s.to[0], s.weight_pt))
        .collect();
    assert_eq!(at_10, vec![(20.0, 40.0, 2.0), (40.0, 60.0, 1.0)]);
    assert_eq!(p.overset_row, None);
    assert!(p.overset_cells.is_empty());
}

/// Hostile grids are refused before any cell is composed: the size gate runs first (a 3x3 grid
/// over a 4-slot limit is TooLarge even though its cell story would also be refused), spans are
/// checked against the grid edge without wrapping (u32::MAX span), overlaps and holes are typed.
#[test]
fn hostile_tables_refused_before_allocation() {
    let doc = document(60.0);
    let body = || row(TableRowType::Body, 10.0);
    let host = story(1, "x", &[(0, 1)]);
    let cells_story = story(2, "abc", &[(0, 3)]);
    let mut missing_font = ShapedParagraph::from_fixed_advance("abc", SourceSpan { start: 0, end: 3 }, 6.0, 12.0, 10.0).unwrap();
    missing_font.font_missing = true;
    let mut grid = Vec::new();
    for r in 0..3 {
        for c in 0..3 {
            grid.push(cell(r, c, (r == 0 && c == 0).then_some((0, 3))));
        }
    }
    let big = table(&[10.0, 10.0, 10.0], vec![body(), body(), body()], grid);
    let req = |t: &StudioTable, limits: TableLimits| {
        layout_table(
            &TableRequest {
                document: &doc,
                table: t,
                host_story: &host,
                host_resolved_text: None,
                cell_story: Some(&cells_story),
                cell_resolved_text: None,
                cell_paragraphs: std::slice::from_ref(&missing_font),
                start: None,
                composer: Composer::FirstFit,
                space: SpaceParams::default(),
                expected_revision: doc.revision,
                limits,
            },
            &TypesetOptions::default(),
            &CancellationToken::default(),
        )
    };
    assert_eq!(req(&big, TableLimits { max_cells: 4, ..TableLimits::default() }), Err(TableError::TooLarge));
    assert_eq!(req(&big, TableLimits::default()), Err(TableError::Layout(LayoutError::MissingFont)));
    let mut span = table(&[10.0], vec![body()], vec![cell(0, 0, None)]);
    span.cells[0].row_span = u32::MAX;
    assert_eq!(req(&span, TableLimits::default()), Err(TableError::Grid("span_overflow")));
    let mut over = table(&[10.0, 10.0], vec![body()], vec![cell(0, 0, None), cell(0, 1, None)]);
    over.cells[0].column_span = 2;
    assert_eq!(req(&over, TableLimits::default()), Err(TableError::Grid("cells_overlap")));
    let hole = table(&[10.0, 10.0], vec![body()], vec![cell(0, 0, None)]);
    assert_eq!(req(&hole, TableLimits::default()), Err(TableError::Grid("cell_missing")));
    let wide = table(&vec![1.0; MAX_TABLE_COLUMNS + 1], vec![body()], vec![]);
    assert_eq!(req(&wide, TableLimits::default()), Err(TableError::TooLarge));
}
