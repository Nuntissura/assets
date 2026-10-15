//! SMB-LAYOUT-LONGDOC: long-document features lowered to PDF through `export_pdf_with` and proven
//! by press `render_page` pixels (1 px per pt, device y down = spread y, no bleed, one 300 x 400
//! page) and by an independent scan of the decoded content streams (glyph ids and positions).
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_interop_layout::layout::{
    Composer, FlowPosition, LayoutGlyph, ShapedParagraph, SourceSpan, SpaceParams, TableLimits, TableRequest, TypesetOptions, layout_table,
};
use hsk_studio_interop_layout::press::{Colour, ExportOptions, Limits, render_page};
use hsk_studio_interop_layout::*;
use hsk_studio_nib::OwnedPath;

/// Inter Regular (OFL); glyph ids and advances (2816 units per em) as in tests/pdf_export.rs.
const INTER: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");
const UPM: f64 = 2816.0;
const SIZE: f64 = 20.0;
const GLYPHS: [(char, u16, f64); 3] = [('H', 161, 2084.0), ('i', 679, 668.0), (' ', 1682, 792.0)];

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
fn edges(v: f64) -> LayoutEdges {
    LayoutEdges { top: len(v), right: len(v), bottom: len(v), left: len(v) }
}
fn on_spread() -> Parent {
    Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: id("SPGS", 1) })
}
/// Text frame layer `n` of story 1 on page-1 at (20, y), 200 x 100, threaded 1 -> 2.
fn frame_layer(n: u32, y: f64) -> StudioLayer {
    StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", n),
        name: format!("frame {n}"),
        parent: on_spread(),
        kind: "text".into(),
        payload: Payload::Text { story_id: id("STXT", 1) },
        visible: true,
        appearance: None,
        text_frame: Some(StudioTextFrame {
            page_id: "page-1".into(),
            bounds: Bounds { x: len(20.0), y: len(y), width: len(200.0), height: len(100.0) },
            inset_spacing: edges(0.0),
            text_column_count: 1,
            text_column_gutter: len(0.0),
            first_baseline_offset: FirstBaseline::Fixed,
            minimum_first_baseline_offset: len(16.0),
            vertical_justification: VerticalJustification::Top,
            auto_sizing_type: AutoSizing::Off,
            previous_text_frame: (n == 2).then(|| id("SLYR", 1)),
            next_text_frame: (n == 1).then(|| id("SLYR", 2)),
        }),
    }
}
fn document(extra: Vec<StudioLayer>) -> StudioDocument {
    let mut layers = vec![frame_layer(1, 20.0), frame_layer(2, 200.0)];
    layers.extend(extra);
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision: 5,
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
            bounds: Bounds { x: len(0.0), y: len(0.0), width: len(300.0), height: len(400.0) },
            pages: vec![StudioPage {
                page_id: "page-1".into(),
                width: len(300.0),
                height: len(400.0),
                side: PageSide::SingleSided,
                margins: edges(10.0),
                bleed: None,
            }],
        }],
        layers,
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    }
}
struct Everything;
impl Resolver for Everything {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Available
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}
fn story(n: u32, text: &str) -> StudioTextStory {
    let end = text.len() as u64;
    StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", n),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.into() },
        paragraphs: vec![StoryParagraph { start: 0, end, style_ref: None, paragraph_direction: None }],
        character_ranges: vec![StoryRange { start: 0, end, style_ref: None, applied_language: Some("en-US".into()) }],
    }
}
fn glyph(c: char) -> (u16, f64) {
    let (_, g, adv) = GLYPHS.iter().copied().find(|(ch, ..)| *ch == c).unwrap();
    (g, adv / UPM * SIZE)
}
/// One Inter paragraph of `text` (line height 24, ascent 16) and its PDF glyphs.
fn shaped(text: &str) -> (ShapedParagraph, Vec<PdfGlyph>) {
    let glyphs: Vec<LayoutGlyph> = text
        .char_indices()
        .map(|(at, c)| LayoutGlyph { cluster: SourceSpan { start: at as u64, end: (at + 1) as u64 }, advance_pt: glyph(c).1 })
        .collect();
    let p = ShapedParagraph::latin_ltr(SourceSpan { start: 0, end: text.len() as u64 }, glyphs, 24.0, 16.0);
    let ids = text.chars().map(|c| PdfGlyph { glyph_id: glyph(c).0, font: 0, size_pt: SIZE, x_offset_pt: 0.0, y_offset_pt: 0.0 }).collect();
    (p, ids)
}
struct NoAssets;
impl PdfAssets for NoAssets {
    fn path(&self, _: &str) -> Option<OwnedPath> {
        None
    }
    fn raster(&self, _: &str) -> Option<PlacedRaster<'_>> {
        None
    }
    fn profile(&self, _: &str) -> Option<&[u8]> {
        None
    }
}
fn streams(pdf: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(s) = find(&pdf[at..], b">>\nstream\n").map(|i| at + i + 10) {
        let Some(e) = find(&pdf[s..], b"\nendstream").map(|i| s + i) else { break };
        if let Ok(d) = miniz_oxide::inflate::decompress_to_vec_zlib(&pdf[s..e]) {
            out.push(d);
        }
        at = e + 10;
    }
    out
}
fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}
/// `(gid, x, y)` of every `1 0 0 1 x y Tm` + `<gggg> Tj` pair of the content streams.
fn shown(pdf: &[u8]) -> Vec<(u16, f64, f64)> {
    let mut out = Vec::new();
    for s in streams(pdf) {
        let text = String::from_utf8_lossy(&s).into_owned();
        let lines: Vec<&str> = text.lines().collect();
        for w in lines.windows(2) {
            let t: Vec<&str> = w[0].split(' ').collect();
            if t.len() == 7 && t[6] == "Tm" && w[1].ends_with(" Tj") && w[1].starts_with('<') {
                out.push((u16::from_str_radix(&w[1][1..5], 16).unwrap(), t[4].parse().unwrap(), t[5].parse().unwrap()));
            }
        }
    }
    out
}
fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(p, q)| p.abs_diff(q) <= 3)
}
fn rgb(r: f64, g: f64, b: f64) -> StudioColor {
    StudioColor { r, g, b, colour_profile_id: ProfileBinding::Unassigned(()) }
}
fn input<'a>(snapshot: &'a Snapshot, stories: &'a [PdfStory<'a>], fonts: &'a [PdfFont<'a>], assets: &'a dyn PdfAssets) -> PdfExportInput<'a> {
    PdfExportInput { snapshot, design: None, type_styles: None, stories, fonts, assets, options: ExportOptions::default(), limits: Limits::default() }
}

/// Slice 1: a table threaded through frames 1 (spread y 20) and 2 (spread y 200), two 100 pt
/// columns. Header row (red fill, "Hi" in its first cell), body rows 1-3 with fills blue, green and
/// none, every row 30 pt (min height; "Hi" needs 24), every cell a 2 pt black bottom stroke. Frame 1
/// (100 pt): header 0..30, rows 1-2 at 30/60; row 3 needs 90..120 -> frame 2 with the header again.
/// Pixels: frame 1 header (170, 35) red, row 1 (70, 65) blue, row 2 (170, 95) green, the stroke at
/// spread y 50 (row boundary 30 + frame 20) black, frame 2 header (170, 215) red, row 3 (70, 245)
/// white. Glyphs: "H" at the header cell's first baseline (16) in both frames: PDF (20, 400 - 36) and
/// (20, 400 - 216).
#[test]
fn table_exports_fills_strokes_and_repeated_header_text() {
    let cancel = CancellationToken::default();
    let doc = document(vec![]);
    let snapshot = validate_document(doc.clone(), Budget::default(), &cancel, &Everything).unwrap();
    let row = |t: TableRowType| StudioTableRow { row_type: t, minimum_height: len(30.0), maximum_height: len(300.0), keep_with_next_row: false, start_row: TableStartRow::Anywhere, allow_split: false };
    let black = || StudioCellStroke { weight: len(2.0), colour: rgb(0.0, 0.0, 0.0), tint: 100.0 };
    let fills = [Some(rgb(1.0, 0.0, 0.0)), Some(rgb(0.0, 0.0, 1.0)), Some(rgb(0.0, 1.0, 0.0)), None];
    let mut cells = Vec::new();
    for r in 0..4u32 {
        for c in 0..2u32 {
            cells.push(StudioTableCell {
                row: r,
                column: c,
                row_span: 1,
                column_span: 1,
                content: (r == 0 && c == 0).then_some(StudioByteRange { start: 0, end: 2 }),
                insets: edges(0.0),
                fill: fills[r as usize].clone(),
                fill_tint: None,
                strokes: StudioCellEdges { bottom: Some(black()), ..StudioCellEdges::default() },
                stroke_priorities: StudioEdgePriorities::default(),
                vertical_justification: VerticalJustification::Top,
            });
        }
    }
    let table = StudioTable {
        table_id: "t1".into(),
        story_id: id("STXT", 1),
        anchor: 0,
        cell_story_id: Some(id("STXT", 2)),
        column_widths: vec![len(100.0), len(100.0)],
        rows: vec![row(TableRowType::Header), row(TableRowType::Body), row(TableRowType::Body), row(TableRowType::Body)],
        cells,
        break_headers: TableHeaderBreak::OncePerTextFrame,
        break_footers: TableHeaderBreak::OncePerTextFrame,
        skip_first_header: false,
        skip_last_footer: false,
        stroke_order: TableStrokeOrder::RowOnTop,
    };
    let host = story(1, "x");
    let cell_story = story(2, "Hi");
    let (p, ids) = shaped("Hi");
    let paragraphs = vec![p];
    let glyphs = vec![ids];
    let proposal = layout_table(
        &TableRequest {
            document: snapshot.document(),
            table: &table,
            host_story: &host,
            host_resolved_text: None,
            cell_story: Some(&cell_story),
            cell_resolved_text: None,
            cell_paragraphs: &paragraphs,
            start: None,
            composer: Composer::FirstFit,
            space: SpaceParams::default(),
            expected_revision: 5,
            limits: TableLimits::default(),
        },
        &TypesetOptions::default(),
        &cancel,
    )
    .unwrap();
    assert_eq!(proposal.parts.len(), 2);
    assert_eq!(proposal.end, Some(FlowPosition { frame: 1, y_pt: 60.0 }));
    let tables = [PdfTable {
        proposal: &proposal,
        cells: PdfStory { story: &cell_story, text: "Hi", placement: &proposal.text, paragraphs: &paragraphs, glyphs: &glyphs, default_fill: Colour::DeviceGray(0.0) },
    }];
    let fonts = [PdfFont { bytes: INTER, face_index: 0, hyphen: None }];
    let out = export_pdf_with(&input(&snapshot, &[], &fonts, &NoAssets), &PdfLongDoc { tables: &tables, ..PdfLongDoc::default() }, &cancel).unwrap();
    let page = render_page(&out.bytes, 0, 1.0, &Limits::default(), &cancel).unwrap();
    let px = |x, y| page.pixel(x, y).unwrap();
    assert!(near(px(170, 35), [255, 0, 0]), "header {:?}", px(170, 35));
    assert!(near(px(70, 65), [0, 0, 255]), "row 1 {:?}", px(70, 65));
    assert!(near(px(170, 95), [0, 255, 0]), "row 2 {:?}", px(170, 95));
    assert!(near(px(170, 50), [0, 0, 0]), "stroke {:?}", px(170, 50));
    assert!(near(px(170, 215), [255, 0, 0]), "repeated header {:?}", px(170, 215));
    assert!(near(px(70, 245), [255, 255, 255]), "row 3 unfilled {:?}", px(70, 245));
    assert!(near(px(250, 65), [255, 255, 255]), "outside the table {:?}", px(250, 65));
    let h = glyph('H').0;
    let hs: Vec<(f64, f64)> = shown(&out.bytes).into_iter().filter(|g| g.0 == h).map(|g| (g.1, g.2)).collect();
    assert_eq!(hs, vec![(20.0, 364.0), (20.0, 184.0)]);
    // Stale or mismatched table placements are refused.
    let mut stale = proposal.clone();
    stale.base_revision = 4;
    let bad = [PdfTable { proposal: &stale, cells: PdfStory { placement: &stale.text, ..tables[0].cells } }];
    let err = export_pdf_with(&input(&snapshot, &[], &fonts, &NoAssets), &PdfLongDoc { tables: &bad, ..PdfLongDoc::default() }, &cancel).unwrap_err();
    assert_eq!(err.code(), "stale_revision");
}
