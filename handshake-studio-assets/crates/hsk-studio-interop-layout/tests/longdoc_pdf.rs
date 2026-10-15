//! SMB-LAYOUT-LONGDOC: long-document features lowered to PDF through `export_pdf_with` and proven
//! by press `render_page` pixels (1 px per pt, device y down = spread y, no bleed, one 300 x 400
//! page) and by an independent scan of the decoded content streams (glyph ids and positions).
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
mod common;
use hsk_studio_interop_layout::layout::{
    Composer, FlowPosition, LayoutGlyph, ReflowRequest, ShapedParagraph, SourceSpan, SpaceParams, TableLimits, TableRequest, TocLimits, TocSource,
    TypesetOptions, generate_toc, layout_table, number_pages, reflow_typeset,
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

fn raster_layer(n: u32) -> StudioLayer {
    StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", n),
        name: format!("picture {n}"),
        parent: on_spread(),
        kind: "raster".into(),
        payload: Payload::Raster { tiles: vec![] },
        visible: true,
        appearance: None,
        text_frame: None,
    }
}
/// A 4 x 2 linear-light tile: columns 0-1 red, columns 2-3 blue, opaque.
struct Picture {
    tile: TileRef,
    profile_id: hsk_studio_accord::DomainId,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Picture {
    fn new() -> Self {
        let px = |x: usize| if x < 2 { [1.0f32, 0.0, 0.0] } else { [0.0, 0.0, 1.0] };
        let colour = (0..2).flat_map(|_| (0..4).flat_map(move |x| px(x).into_iter().flat_map(f32::to_le_bytes))).collect();
        let alpha = (0..8).flat_map(|_| 1.0f32.to_le_bytes()).collect();
        let tile = TileRef {
            object_key: "t0".into(),
            layer_id: id("SLYR", 3),
            column: 0,
            row: 0,
            width: Length { value: 4.0, unit: GeometryUnit::Px },
            height: Length { value: 2.0, unit: GeometryUnit::Px },
            format: "rgb_f32le".into(),
            colour_profile_id: id("SCPF", 1),
            schema_id: TileSchema::V1,
            content_digest: ContentDigest { algorithm: "sha256".into(), digest: "00".into() },
            artifact_manifest_id: "manifest-1".into(),
        };
        Self { tile, profile_id: hsk_studio_accord::DomainId::parse(&id("SCPF", 1)).unwrap(), colour, alpha }
    }
}
impl PdfAssets for Picture {
    fn path(&self, _: &str) -> Option<OwnedPath> {
        None
    }
    /// The same picture for layers 3 and 4; the origin is ignored for graphic frames.
    fn raster(&self, layer_id: &str) -> Option<PlacedRaster<'_>> {
        use hsk_studio_prism::{ProfileInput, WORKING_PROFILE_LINEAR_SRGB, profile_hash};
        (layer_id == id("SLYR", 3) || layer_id == id("SLYR", 4)).then(|| PlacedRaster {
            grid: hsk_studio_pigment::Grid { origin_x: 0, origin_y: 0, tile_width: 4, tile_height: 2 },
            tiles: vec![hsk_studio_pigment::ResolvedTile {
                tile_ref: &self.tile,
                bounds: hsk_studio_pigment::Rect { x: 0, y: 0, width: 4, height: 2 },
                sample_format: "rgb_f32le",
                colour_bytes: &self.colour,
                colour_stride_bytes: 48,
                colour_sha256: profile_hash(&self.colour),
                alpha_bytes: &self.alpha,
                alpha_stride_bytes: 16,
                alpha_sha256: profile_hash(&self.alpha),
                transfer: "linear_light",
                alpha_association: "straight",
                profile: ProfileInput { profile_id: &self.profile_id, bytes: WORKING_PROFILE_LINEAR_SRGB, expected_sha256: profile_hash(WORKING_PROFILE_LINEAR_SRGB) },
            }],
            origin_pt: [0.0, 0.0],
            pt_per_px: 1.0,
        })
    }
    fn profile(&self, _: &str) -> Option<&[u8]> {
        None
    }
}

/// Slice 2: the 4 x 2 picture as 200 x 100 pt content in two 100 x 100 frames on page 1.
/// Frame A (20, 120), rectangle, proportionally: s = 0.5, box 100 x 50 centred at y 145..195 ->
/// (40, 170) red, (100, 170) blue, (70, 130) white letterbox. Frame B (150, 120), inscribed ellipse,
/// fill_proportionally: s = 1, box 200 x 100 at x 100..300, so the frame shows content x 50..150:
/// (170, 170) red, (230, 170) blue; (125, 170) is content but outside the frame (white) and the
/// frame corner (153, 123) is outside the ellipse (white). Layer 3 without a graphic frame record
/// would draw at its PlacedRaster origin; the records replace that placement.
#[test]
fn placed_graphics_fit_and_clip_to_frame_shapes() {
    let cancel = CancellationToken::default();
    let doc = document(vec![raster_layer(3), raster_layer(4)]);
    let snapshot = validate_document(doc, Budget::default(), &cancel, &Everything).unwrap();
    let frame = |layer: u32, x: f64, shape: StudioFrameShape, fit: StudioFit| StudioGraphicFrame {
        layer_id: id("SLYR", layer),
        page_id: "page-1".into(),
        bounds: Bounds { x: len(x), y: len(120.0), width: len(100.0), height: len(100.0) },
        shape,
        content_width: len(200.0),
        content_height: len(100.0),
        fit: Some(fit),
        frame_fitting: None,
        anchor: StudioFitAnchor::Center,
        crop: None,
    };
    let mut records = StudioLongDocRecords::new(id("SDOC", 1));
    records.graphic_frames.push(frame(3, 20.0, StudioFrameShape::Rectangle, StudioFit::Proportionally));
    records.graphic_frames.push(frame(4, 150.0, StudioFrameShape::Ellipse, StudioFit::FillProportionally));
    assert!(validate_longdoc_records(&records, snapshot.document(), Budget::default(), &cancel, &Everything).unwrap().is_empty());
    let placed: Vec<_> = records.graphic_frames.iter().map(|g| hsk_studio_interop_layout::layout::place_graphic(snapshot.document(), g).unwrap()).collect();
    let picture = Picture::new();
    let out = export_pdf_with(&input(&snapshot, &[], &[], &picture), &PdfLongDoc { graphics: &placed, ..PdfLongDoc::default() }, &cancel).unwrap();
    assert_eq!(out.tiles_drawn, 2);
    let page = render_page(&out.bytes, 0, 1.0, &Limits::default(), &cancel).unwrap();
    let px = |x, y| page.pixel(x, y).unwrap();
    let close = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(p, q)| p.abs_diff(q) <= 6);
    assert!(close(px(40, 170), [255, 0, 0]), "A red {:?}", px(40, 170));
    assert!(close(px(100, 170), [0, 0, 255]), "A blue {:?}", px(100, 170));
    assert!(close(px(70, 130), [255, 255, 255]), "A letterbox {:?}", px(70, 130));
    assert!(close(px(170, 170), [255, 0, 0]), "B red {:?}", px(170, 170));
    assert!(close(px(230, 170), [0, 0, 255]), "B blue {:?}", px(230, 170));
    assert!(close(px(125, 170), [255, 255, 255]), "B clipped to the frame {:?}", px(125, 170));
    assert!(close(px(153, 123), [255, 255, 255]), "B clipped to the ellipse {:?}", px(153, 123));
    let mut bad = placed.clone();
    bad[0].transform[0] = f64::NAN;
    let err = export_pdf_with(&input(&snapshot, &[], &[], &picture), &PdfLongDoc { graphics: &bad, ..PdfLongDoc::default() }, &cancel).unwrap_err();
    assert_eq!(err.to_string(), "invalid_input: graphic_placement");
}

/// Three single-page spreads b1..b3 (300 x 300). Text layers: 11..13 thread story 5 through one
/// 200 x 30 frame per page (one 24 pt line each); 14 holds story 9 (the TOC) on b1 at y 100.
fn book() -> StudioDocument {
    let page_spread = |n: u32| StudioPageSpread {
        schema_id: PageSpreadSchema::V1,
        page_spread_id: id("SPGS", n),
        name: format!("spread {n}"),
        parent: Parent::Root(()),
        bounds: Bounds { x: len(0.0), y: len(0.0), width: len(300.0), height: len(300.0) },
        pages: vec![StudioPage { page_id: format!("b{n}"), width: len(300.0), height: len(300.0), side: PageSide::SingleSided, margins: edges(10.0), bleed: None }],
    };
    let text_layer = |n: u32, spread: u32, story: u32, y: f64, h: f64, prev: Option<u32>, next: Option<u32>| StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", n),
        name: format!("frame {n}"),
        parent: Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: id("SPGS", spread) }),
        kind: "text".into(),
        payload: Payload::Text { story_id: id("STXT", story) },
        visible: true,
        appearance: None,
        text_frame: Some(StudioTextFrame {
            page_id: format!("b{spread}"),
            bounds: Bounds { x: len(20.0), y: len(y), width: len(200.0), height: len(h) },
            inset_spacing: edges(0.0),
            text_column_count: 1,
            text_column_gutter: len(0.0),
            first_baseline_offset: FirstBaseline::Fixed,
            minimum_first_baseline_offset: len(16.0),
            vertical_justification: VerticalJustification::Top,
            auto_sizing_type: AutoSizing::Off,
            previous_text_frame: prev.map(|p| id("SLYR", p)),
            next_text_frame: next.map(|q| id("SLYR", q)),
        }),
    };
    let mut doc = document(vec![]);
    doc.page_spreads = vec![page_spread(1), page_spread(2), page_spread(3)];
    doc.layers = vec![
        text_layer(11, 1, 5, 10.0, 30.0, None, Some(12)),
        text_layer(12, 2, 5, 10.0, 30.0, Some(11), Some(13)),
        text_layer(13, 3, 5, 10.0, 30.0, Some(12), None),
        text_layer(14, 1, 9, 100.0, 150.0, None, None),
    ];
    doc
}
fn stys(n: u32) -> String {
    id("STYS", n)
}
/// Story `n` of `text` with paragraphs at `paras` (style refs) and its Type shaping (Inter 20 pt,
/// line height 24, ascent 16): each paragraph's visible text is shaped on its own and its clusters
/// moved to the paragraph start.
fn shaped_story(n: u32, text: &str, paras: &[(u64, u64, Option<String>)], files: &[common::text_shaping::FontFile], fonts: &[PdfFont<'_>]) -> (StudioTextStory, Vec<ShapedParagraph>, Vec<Vec<PdfGlyph>>) {
    let mut st = story(n, text);
    st.paragraphs = paras.iter().map(|(start, end, s)| StoryParagraph { start: *start, end: *end, style_ref: s.clone(), paragraph_direction: None }).collect();
    let mut shaped = Vec::new();
    let mut glyphs = Vec::new();
    for (start, end, _) in paras {
        let visible = text[*start as usize..*end as usize].trim_end_matches('\n');
        let segs = [common::text_shaping::Seg { start: 0, end: visible.len() as u64, font: 0, script: *b"Latn", language: "en", axes: vec![] }];
        let (mut p, g) = common::text_shaping::shape(files, visible, &segs, false, SIZE, |prepared| {
            let p = &prepared.paragraphs()[0];
            (ShapedParagraph::from_type(p, 24.0, 16.0), pdf_glyphs_from_type(p, fonts).unwrap())
        });
        for lg in &mut p.glyphs {
            lg.cluster = SourceSpan { start: lg.cluster.start + start, end: lg.cluster.end + start };
        }
        p.source = SourceSpan { start: *start, end: *end };
        shaped.push(p);
        glyphs.push(g);
    }
    (st, shaped, glyphs)
}

/// Slice 3: sections number b1 "i" (lower roman), b2 "S5" (arabic from 5, prefix S), b3 "S6"; the
/// body story puts Alpha (H1) on b1, Beta (H2) on b2, Gamma (H1) on b3. The TOC generated from H1
/// (level 1) and H2 (level 2) reads "Contents / Alpha i / Beta S5 / Gamma S6", is composed into
/// frame 14 and exported: its four lines (PDF baselines 300 - 116, -140, -164, -188) draw exactly
/// the Type glyph ids of those paragraphs, in order, and leave ink in the frame.
#[test]
fn sections_and_toc_export_page_labels() {
    let cancel = CancellationToken::default();
    let snapshot = validate_document(book(), Budget::default(), &cancel, &Everything).unwrap();
    let doc = snapshot.document();
    let files = [common::text_shaping::FontFile::inter()];
    let fonts = [PdfFont { bytes: &files[0].bytes, face_index: 0, hyphen: None }];
    let body = "Alpha\nBeta\nGamma";
    let (body_story, body_paras, body_glyphs) = shaped_story(5, body, &[(0, 6, Some(stys(1))), (6, 11, Some(stys(2))), (11, 16, Some(stys(1)))], &files, &fonts);
    let reflow = |story: &StudioTextStory, paragraphs: &[ShapedParagraph]| {
        reflow_typeset(
            &ReflowRequest {
                document: doc,
                story,
                resolved_text: None,
                paragraphs,
                obstacles: &[],
                composer: Composer::FirstFit,
                space: SpaceParams::default(),
                expected_revision: doc.revision,
                limits: hsk_studio_interop_layout::layout::Limits::default(),
            },
            &TypesetOptions::default(),
            &cancel,
        )
        .unwrap()
    };
    let body_place = reflow(&body_story, &body_paras);
    assert_eq!(body_place.frames.iter().map(|f| f.lines.len()).collect::<Vec<_>>(), [1, 1, 1]);
    let mut b = StudioSection {
        section_id: "b".into(),
        name: "Body".into(),
        page_start: "b2".into(),
        continue_numbering: false,
        page_number_start: Some(5),
        page_number_style: StudioNumberStyle::Arabic,
        include_section_prefix: true,
        section_prefix: Some("S".into()),
        marker: None,
    };
    let a = StudioSection { section_id: "a".into(), name: "Front".into(), page_start: "b1".into(), continue_numbering: true, page_number_start: None, page_number_style: StudioNumberStyle::LowerRoman, include_section_prefix: false, section_prefix: None, marker: None };
    let pages = number_pages(doc, &[a.clone(), b.clone()]).unwrap();
    assert_eq!(pages.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(), ["i", "S5", "S6"]);
    let entry = |s: u32, level: u32| StudioTocEntry { paragraph_style: stys(s), format_style: None, level, page_number_position: TocPageNumberPosition::After, separator: " ".into(), sort_alphabet: false };
    let toc = StudioTocStyle { toc_id: "toc".into(), name: "Contents".into(), title: "Contents".into(), title_style: None, include_overset: false, entries: vec![entry(1, 1), entry(2, 2)] };
    let generated = generate_toc(doc, &toc, &[TocSource { story: &body_story, text: body, placement: Some(&body_place) }], &pages, &id("STXT", 9), TocLimits::default(), &cancel).unwrap();
    assert_eq!(generated.text, "Contents\nAlpha i\nBeta S5\nGamma S6");
    let ranges: Vec<(u64, u64, Option<String>)> = generated.paragraphs.iter().map(|p| (p.start, p.end, None)).collect();
    let (toc_story, toc_paras, toc_glyphs) = shaped_story(9, &generated.text, &ranges, &files, &fonts);
    let toc_place = reflow(&toc_story, &toc_paras);
    assert_eq!(toc_place.frames[0].lines.len(), 4);
    let stories = [
        PdfStory { story: &body_story, text: body, placement: &body_place, paragraphs: &body_paras, glyphs: &body_glyphs, default_fill: Colour::DeviceGray(0.0) },
        PdfStory { story: &toc_story, text: &generated.text, placement: &toc_place, paragraphs: &toc_paras, glyphs: &toc_glyphs, default_fill: Colour::DeviceGray(0.0) },
    ];
    let out = export_pdf_with(&input(&snapshot, &stories, &fonts, &NoAssets), &PdfLongDoc::default(), &cancel).unwrap();
    assert_eq!(out.pages, ["b1", "b2", "b3"]);
    let toc_lines: Vec<(u16, f64)> = shown(&out.bytes).into_iter().filter(|g| g.2 <= 184.0 + 1e-9).map(|g| (g.0, g.2)).collect();
    let expected: Vec<u16> = toc_glyphs.iter().flatten().map(|g| g.glyph_id).collect();
    assert_eq!(toc_lines.iter().map(|g| g.0).collect::<Vec<_>>(), expected);
    let mut ys: Vec<f64> = toc_lines.iter().map(|g| g.1).collect();
    ys.dedup();
    assert_eq!(ys, [184.0, 160.0, 136.0, 112.0]);
    let page = render_page(&out.bytes, 0, 1.0, &Limits::default(), &cancel).unwrap();
    let ink = (100..250).flat_map(|y| (20..220).map(move |x| (x, y))).filter(|(x, y)| page.pixel(*x, *y) != Some([255, 255, 255])).count();
    assert!(ink > 200, "TOC ink {ink}");
    // The same TOC with a continuing second section: b2 ii, b3 iii.
    b.continue_numbering = true;
    b.page_number_start = None;
    b.page_number_style = StudioNumberStyle::LowerRoman;
    b.include_section_prefix = false;
    let pages = number_pages(doc, &[a, b]).unwrap();
    let again = generate_toc(doc, &toc, &[TocSource { story: &body_story, text: body, placement: Some(&body_place) }], &pages, &id("STXT", 9), TocLimits::default(), &cancel).unwrap();
    assert_eq!(again.text, "Contents\nAlpha i\nBeta ii\nGamma iii");
}

/// One 300 x 400 page; frame 21 (story 7) at (20, 20) 200 x 200, frame 22 (story 8) at (20, 230)
/// 200 x 120.
fn notes_document() -> StudioDocument {
    let mut doc = book();
    doc.page_spreads.truncate(1);
    doc.page_spreads[0].pages[0].height = len(400.0);
    doc.page_spreads[0].bounds.height = len(400.0);
    let frame = |n: u32, story: u32, y: f64, h: f64| {
        let mut l = frame_layer(n, y);
        l.parent = Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: id("SPGS", 1) });
        l.payload = Payload::Text { story_id: id("STXT", story) };
        let f = l.text_frame.as_mut().unwrap();
        f.page_id = "b1".into();
        f.bounds.height = len(h);
        f.previous_text_frame = None;
        f.next_text_frame = None;
        l
    };
    doc.layers = vec![frame(21, 7, 20.0, 200.0), frame(22, 8, 230.0, 120.0)];
    doc
}

/// Slice 4: body "Body note" (one line, baseline 16 -> PDF y 400 - 36 = 364) with a
/// footnote at the story end; the note "The footnote." (24 pt) sits at the frame bottom: band top
/// 200 - 24 = 176, baseline 192 -> PDF y 400 - 212 = 188; the 1 pt rule at band top - 2.5 = 173.5 ->
/// spread y 193.5 (pixel row 193 dark at x 20..70). The reference marker "1" (12 pt overlay) is
/// drawn at layout's reference position raised 6 pt. The index generated from marks on "Body"
/// and "note" ("Index / Body, 1 / Notes / footnote, 1") is composed into frame 22 and drawn line by
/// line (PDF baselines 154, 130, 106, 82) with exactly its Type glyph ids.
#[test]
fn footnotes_and_index_export() {
    let cancel = CancellationToken::default();
    let snapshot = validate_document(notes_document(), Budget::default(), &cancel, &Everything).unwrap();
    let doc = snapshot.document();
    let files = [common::text_shaping::FontFile::inter()];
    let fonts = [PdfFont { bytes: &files[0].bytes, face_index: 0, hyphen: None }];
    let body = "Body note";
    let (body_story, body_paras, body_glyphs) = shaped_story(7, body, &[(0, 9, None)], &files, &fonts);
    let note_text = "The footnote.";
    let (note_story, note_paras, note_glyphs) = shaped_story(9, note_text, &[(0, 13, None)], &files, &fonts);
    let footnote = StudioFootnote { note_id: "n1".into(), story_id: id("STXT", 7), anchor: 9, note_story_id: id("STXT", 9) };
    let black = rgb(0.0, 0.0, 0.0);
    let options = StudioFootnoteOptions {
        numbering_style: StudioNumberStyle::Arabic,
        start_at: 1,
        restart: FootnoteRestart::DontRestart,
        prefix: String::new(),
        suffix: String::new(),
        space_between: len(2.0),
        spacer: len(6.0),
        rule: Some(StudioFootnoteRule { weight: len(1.0), colour: black.clone(), tint: 100.0, left_indent: len(0.0), width: len(50.0), offset: len(2.5) }),
        continuing_rule: None,
        eos_placement: false,
        no_splitting: false,
    };
    let notes = [hsk_studio_interop_layout::layout::NoteInput { note: &footnote, story: &note_story, resolved_text: None, paragraphs: &note_paras }];
    let request = hsk_studio_interop_layout::layout::ReflowRequest {
        document: doc,
        story: &body_story,
        resolved_text: None,
        paragraphs: &body_paras,
        obstacles: &[],
        composer: Composer::FirstFit,
        space: SpaceParams::default(),
        expected_revision: doc.revision,
        limits: hsk_studio_interop_layout::layout::Limits::default(),
    };
    let fp = hsk_studio_interop_layout::layout::layout_footnotes(
        &hsk_studio_interop_layout::layout::FootnoteRequest { reflow: request, notes: &notes, options: &options, pages: &[], max_iterations: 8 },
        &TypesetOptions::default(),
        &cancel,
    )
    .unwrap();
    assert_eq!((fp.iterations, fp.bands.clone()), (2, vec![30.0]));
    let n = &fp.notes[0];
    assert_eq!((n.label.as_str(), n.reference_frame), ("1", 0));
    // The reference marker: Type-shaped "1" at 12 pt on the reference, raised 6 pt.
    let (marker_story, mut marker_paras, marker_glyphs) = shaped_story(10, "1", &[(0, 1, None)], &files, &fonts);
    let marker_glyphs: Vec<Vec<PdfGlyph>> = marker_glyphs.into_iter().map(|g| g.into_iter().map(|g| PdfGlyph { size_pt: 12.0, ..g }).collect()).collect();
    for g in &mut marker_paras[0].glyphs {
        g.advance_pt *= 0.6;
    }
    let body_frame = &fp.body.frames[0];
    let marker_place = hsk_studio_interop_layout::layout::LayoutProposal {
        base_revision: doc.revision,
        frames: vec![hsk_studio_interop_layout::layout::FramePlacement {
            lines: vec![hsk_studio_interop_layout::layout::Line {
                paragraph: 0,
                baseline_y_pt: n.reference[1] - 6.0,
                x_start_pt: n.reference[0],
                width_pt: 0.0,
                available_pt: 0.0,
                hang_pt: 0.0,
                source: SourceSpan { start: 0, end: 1 },
                glyph_range: 0..1,
                overfull: false,
                ratio: None,
                hyphenated: false,
                justification: None,
            }],
            ..body_frame.clone()
        }],
        overset: None,
    };
    // Index from marks on the body story.
    let pages = number_pages(doc, &[]).unwrap();
    let mark = |m: &str, at: u64, topics: &[&str]| StudioIndexMark { mark_id: m.into(), story_id: id("STXT", 7), at, topics: topics.iter().map(|t| t.to_string()).collect(), sort_by: vec![], xref: None };
    let index_options = StudioIndexOptions {
        title: "Index".into(),
        title_style: None,
        index_format: IndexFormat::Nested,
        include_section_headings: false,
        include_empty_index_sections: false,
        level_styles: vec![],
        section_heading_style: None,
        following_topic_separator: ", ".into(),
        between_entries_separator: "; ".into(),
        page_range_separator: "-".into(),
        between_page_numbers_separator: ", ".into(),
        before_cross_reference_separator: ". ".into(),
        entry_end_separator: String::new(),
    };
    let index = hsk_studio_interop_layout::layout::generate_index(
        doc,
        &index_options,
        &[mark("m1", 0, &["Body"]), mark("m2", 5, &["Notes", "footnote"])],
        &[TocSource { story: &body_story, text: body, placement: Some(&fp.body) }],
        &pages,
        None,
        hsk_studio_interop_layout::layout::IndexLimits::default(),
        &cancel,
    )
    .unwrap();
    assert_eq!(index.story.text, "Index\nBody, 1\nNotes\nfootnote, 1");
    let ranges: Vec<(u64, u64, Option<String>)> = index.story.paragraphs.iter().map(|p| (p.start, p.end, None)).collect();
    let (index_story, index_paras, index_glyphs) = shaped_story(8, &index.story.text, &ranges, &files, &fonts);
    let index_place = reflow_typeset(
        &ReflowRequest { story: &index_story, paragraphs: &index_paras, ..request },
        &TypesetOptions::default(),
        &cancel,
    )
    .unwrap();
    assert!(index_place.overset.is_none());
    let stories = [
        PdfStory { story: &body_story, text: body, placement: &fp.body, paragraphs: &body_paras, glyphs: &body_glyphs, default_fill: Colour::DeviceGray(0.0) },
        PdfStory { story: &index_story, text: &index.story.text, placement: &index_place, paragraphs: &index_paras, glyphs: &index_glyphs, default_fill: Colour::DeviceGray(0.0) },
    ];
    let overlays = [
        PdfStory { story: &note_story, text: note_text, placement: &fp.note_text[0].1, paragraphs: &note_paras, glyphs: &note_glyphs, default_fill: Colour::DeviceGray(0.0) },
        PdfStory { story: &marker_story, text: "1", placement: &marker_place, paragraphs: &marker_paras, glyphs: &marker_glyphs, default_fill: Colour::DeviceGray(0.0) },
    ];
    let out = export_pdf_with(&input(&snapshot, &stories, &fonts, &NoAssets), &PdfLongDoc { overlays: &overlays, rules: &fp.rules, ..PdfLongDoc::default() }, &cancel).unwrap();
    let g = shown(&out.bytes);
    let at_y = |y: f64| -> Vec<u16> { g.iter().filter(|s| (s.2 - y).abs() < 1e-6).map(|s| s.0).collect() };
    assert_eq!(at_y(364.0), body_glyphs[0].iter().map(|x| x.glyph_id).collect::<Vec<_>>(), "body line");
    assert_eq!(at_y(188.0), note_glyphs[0].iter().map(|x| x.glyph_id).collect::<Vec<_>>(), "note at the frame bottom");
    let marker_y = 400.0 - (20.0 + n.reference[1] - 6.0);
    let marker: Vec<(u16, f64)> = g.iter().filter(|s| (s.2 - marker_y).abs() < 1e-6).map(|s| (s.0, s.1)).collect();
    assert_eq!(marker, vec![(marker_glyphs[0][0].glyph_id, 20.0 + n.reference[0])]);
    let index_ids: Vec<u16> = [154.0, 130.0, 106.0, 82.0].iter().flat_map(|y| at_y(*y)).collect();
    assert_eq!(index_ids, index_glyphs.iter().flatten().map(|x| x.glyph_id).collect::<Vec<_>>());
    let page = render_page(&out.bytes, 0, 1.0, &Limits::default(), &cancel).unwrap();
    assert!(near(page.pixel(45, 193).unwrap(), [0, 0, 0]), "rule {:?}", page.pixel(45, 193));
    assert!(near(page.pixel(100, 193).unwrap(), [255, 255, 255]), "rule ends at 70 {:?}", page.pixel(100, 193));
}
