//! SMB-LAYOUT-UI slice 2 in PDF: one story with an inline anchored picture, a cross-reference
//! written into its second paragraph and a hidden conditional range, composed by layout and
//! exported through `export_pdf_with`. Proven by an independent scan of the content streams (glyph
//! ids and positions; tests/common/pdf_scan.rs) and press `render_page` pixels (1 px per pt,
//! device y down = spread y, one 300 x 400 page).
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
mod common;
use hsk_studio_interop_layout::layout::{
    Composer, LayoutGlyph, ReflowRequest, ShapedParagraph, SourceSpan, SpaceParams, TypesetOptions, XrefContext, XrefStatus, XrefStory,
    anchored_graphic, conditional_view, inline_adjusted_paragraphs, place_anchored_objects, place_graphic, reflow_flow, resolve_cross_references, write_cross_reference,
};
use hsk_studio_interop_layout::press::{Colour, ExportOptions, Limits, render_page};
use hsk_studio_interop_layout::*;
use hsk_studio_nib::OwnedPath;

const INTER: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");
const UPM: f64 = 2816.0;
const SIZE: f64 = 20.0;
/// Inter glyph ids and advances; the object-replacement placeholder is drawn as a space.
const GLYPHS: [(char, u16, f64); 4] = [('H', 161, 2084.0), ('i', 679, 668.0), (' ', 1682, 792.0), ('\u{FFFC}', 1682, 792.0)];
const SPACE_GID: u16 = 1682;

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
fn document() -> StudioDocument {
    let frame = StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", 1),
        name: "frame".into(),
        parent: on_spread(),
        kind: "text".into(),
        payload: Payload::Text { story_id: id("STXT", 1) },
        visible: true,
        appearance: None,
        text_frame: Some(StudioTextFrame {
            page_id: "page-1".into(),
            bounds: Bounds { x: len(20.0), y: len(20.0), width: len(200.0), height: len(100.0) },
            inset_spacing: edges(0.0),
            text_column_count: 1,
            text_column_gutter: len(0.0),
            first_baseline_offset: FirstBaseline::Fixed,
            minimum_first_baseline_offset: len(16.0),
            vertical_justification: VerticalJustification::Top,
            auto_sizing_type: AutoSizing::Off,
            previous_text_frame: None,
            next_text_frame: None,
        }),
    };
    let picture = StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", 3),
        name: "picture".into(),
        parent: on_spread(),
        kind: "raster".into(),
        payload: Payload::Raster { tiles: vec![] },
        visible: true,
        appearance: None,
        text_frame: None,
    };
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
        layers: vec![frame, picture],
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
    fn raster(&self, layer_id: &str) -> Option<PlacedRaster<'_>> {
        use hsk_studio_prism::{ProfileInput, WORKING_PROFILE_LINEAR_SRGB, profile_hash};
        (layer_id == id("SLYR", 3)).then(|| PlacedRaster {
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
fn glyph(c: char) -> (u16, f64) {
    let (_, g, adv) = GLYPHS.iter().copied().find(|(ch, ..)| *ch == c).unwrap();
    (g, adv / UPM * SIZE)
}
fn story(text: &str, paras: &[(u64, u64)]) -> StudioTextStory {
    let end = text.len() as u64;
    StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", 1),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.into() },
        paragraphs: paras.iter().map(|&(s, e)| StoryParagraph { start: s, end: e, style_ref: None, paragraph_direction: None }).collect(),
        character_ranges: vec![StoryRange { start: 0, end, style_ref: None, applied_language: Some("en-US".into()) }],
    }
}
/// Inter paragraphs (line height 24, ascent 16) over the given spans and their PDF glyphs.
fn shaped(text: &str, spans: &[(u64, u64)]) -> (Vec<ShapedParagraph>, Vec<Vec<PdfGlyph>>) {
    spans
        .iter()
        .map(|&(s, e)| {
            let slice = &text[s as usize..e as usize];
            let glyphs: Vec<LayoutGlyph> = slice
                .char_indices()
                .map(|(at, c)| LayoutGlyph {
                    cluster: SourceSpan { start: s + at as u64, end: s + (at + c.len_utf8()) as u64 },
                    advance_pt: glyph(c).1,
                })
                .collect();
            let ids = slice.chars().map(|c| PdfGlyph { glyph_id: glyph(c).0, font: 0, size_pt: SIZE, x_offset_pt: 0.0, y_offset_pt: 0.0 }).collect();
            (ShapedParagraph::latin_ltr(SourceSpan { start: s, end: e }, glyphs, 24.0, 16.0), ids)
        })
        .unzip()
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

/// Original story "Hi iii Hi\u{FFFC}H\nX" (P0 0..14, P1 14..15). The cross-reference x1 replaces
/// "X" (14..15) by the first word of P0 ("Hi"); condition "web" (hidden) tags "iii " (3..7); the
/// picture is anchored inline at the placeholder (byte 9), 20 x 10 pt. Composition of the visible
/// story "Hi Hi\u{FFFC}H\nHi" in the frame at (20, 20), first baseline 16, line height 24:
/// line 0 at spread y 36 (PDF 364): H 20, i 20+h, space, H, i, placeholder, H after the 20 pt
/// anchor advance; line 1 at y 60 (PDF 340): H 20, i 20+h. The picture sits on line 0's baseline at
/// the placeholder x: red over its left half, blue over its right half.
#[test]
fn anchored_picture_cross_reference_and_hidden_condition_export() {
    let cancel = CancellationToken::default();
    let snapshot = validate_document(document(), Budget::default(), &cancel, &Everything).unwrap();
    let doc = snapshot.document();
    let original = "Hi iii Hi\u{FFFC}H\nX";
    let st = story(original, &[(0, 14), (14, 15)]);
    let mut flow = StudioLayoutFlowRecords::new(id("SDOC", 1));
    flow.conditions.push(StudioCondition {
        condition_id: "web".into(),
        name: "Web".into(),
        visible: false,
        indicator_colour: StudioRgba { r: 0.0, g: 0.0, b: 1.0, a: 1.0, colour_profile_id: ProfileBinding::Unassigned(()) },
        indicator_method: StudioIndicatorMethod::Highlight,
        underline_indicator_appearance: StudioUnderlineAppearance::Solid,
    });
    flow.conditional_ranges.push(StudioConditionalRange { story_id: id("STXT", 1), start: 3, end: 7, conditions: vec!["web".into()] });
    flow.insert_anchored_object(&id("SLYR", 3), &id("STXT", 1), 9, StudioAnchorPosition::Inline, [len(20.0), len(10.0)]).unwrap();
    flow.xref_formats.push(StudioXrefFormat { format_id: "word".into(), name: "First word".into(), definition: r#"<fullPara delim=" "/>"#.into(), character_style: None });
    flow.cross_references.push(StudioCrossReference {
        xref_id: "x1".into(),
        source_story_id: id("STXT", 1),
        source_start: 14,
        source_end: 15,
        destination: StudioXrefDestination::Paragraph { story_id: id("STXT", 1), paragraph_start: 0 },
        format_id: "word".into(),
        resolved_text: None,
    });
    validate_layout_flow_records(&flow, doc, &cancel).unwrap();
    // The flow records travel with the document as JSON (unknown members refused).
    let wire = serde_json::to_value(&flow).unwrap();
    assert_eq!(serde_json::from_value::<StudioLayoutFlowRecords>(wire.clone()).unwrap(), flow);
    let mut bad = wire;
    bad["frame_optionz"] = serde_json::json!([]);
    assert!(serde_json::from_value::<StudioLayoutFlowRecords>(bad).is_err());

    // Cross-reference: resolve, write into the source range.
    let ctx = XrefContext { file_name: "doc", chapter_number: None, pages: &[], type_styles: None };
    let r = resolve_cross_references(&flow, &[XrefStory { story: &st, text: original, placement: None }], &ctx).unwrap();
    assert_eq!((r[0].text.as_deref(), r[0].status), (Some("Hi"), XrefStatus::Stale));
    let written = write_cross_reference(&mut flow, &st, None, "x1", "Hi").unwrap();
    // Conditional visibility resolves before composition; records move into the view.
    let view = conditional_view(&written, None, &flow).unwrap();
    let StoryContent::Inline { text_utf8: visible } = &view.story.content else { unreachable!() };
    assert_eq!(visible, "Hi Hi\u{FFFC}H\nHi");
    let vflow = view.remap_records(&flow);
    assert_eq!(vflow.anchored_objects[0].anchor_byte, 5);
    let (paras, glyphs) = shaped(visible, &[(0, 9), (10, 12)]);
    let request = ReflowRequest {
        document: doc,
        story: &view.story,
        resolved_text: None,
        paragraphs: &paras,
        obstacles: &[],
        composer: Composer::FirstFit,
        space: SpaceParams::default(),
        expected_revision: doc.revision,
        limits: hsk_studio_interop_layout::layout::Limits::default(),
    };
    let placement = reflow_flow(&request, &TypesetOptions::default(), &vflow, &cancel).unwrap();
    let anchors = place_anchored_objects(doc, &vflow, &view.story, &paras, &placement).unwrap();
    let frame = StudioGraphicFrame {
        layer_id: id("SLYR", 3),
        page_id: "page-1".into(),
        bounds: Bounds { x: len(200.0), y: len(300.0), width: len(20.0), height: len(10.0) },
        shape: StudioFrameShape::Rectangle,
        content_width: len(20.0),
        content_height: len(10.0),
        fit: None,
        frame_fitting: None,
        anchor: StudioFitAnchor::TopLeft,
        crop: None,
    };
    let graphics = [anchored_graphic(&place_graphic(doc, &frame).unwrap(), &anchors.placed[0])];

    // The painter gets the glyph runs layout composed (inline anchor advance applied).
    let drawn = inline_adjusted_paragraphs(&vflow, &view.story, &paras).unwrap();
    assert_eq!(drawn[0].glyphs[5].advance_pt, 20.0);
    let stories = [PdfStory { story: &view.story, text: visible, placement: &placement, paragraphs: &drawn, glyphs: &glyphs, default_fill: Colour::DeviceGray(0.0) }];
    let fonts = [PdfFont { bytes: INTER, face_index: 0, hyphen: None }];
    let picture = Picture::new();
    let input = PdfExportInput { snapshot: &snapshot, design: None, type_styles: None, stories: &stories, fonts: &fonts, assets: &picture, options: ExportOptions::default(), limits: Limits::default() };
    let out = export_pdf_with(&input, &PdfLongDoc { graphics: &graphics, ..PdfLongDoc::default() }, &cancel).unwrap();
    assert_eq!(out.tiles_drawn, 1);

    let (h, i, s) = (glyph('H').1, glyph('i').1, glyph(' ').1);
    let x_anchor = 20.0 + h + i + s + h + i;
    let expect = [
        (161, 20.0, 364.0),
        (679, 20.0 + h, 364.0),
        (161, 20.0 + h + i + s, 364.0),
        (679, 20.0 + 2.0 * h + i + s, 364.0),
        (161, x_anchor + 20.0, 364.0),
        (161, 20.0, 340.0),
        (679, 20.0 + h, 340.0),
    ];
    let shown: Vec<(u16, f64, f64)> = common::pdf_scan::shown_runs(&streams(&out.bytes), &hsk_studio_interop_layout::press::font_advance_widths(INTER, 0).unwrap())
        .into_iter()
        .filter(|g| g.0 != SPACE_GID)
        .collect();
    assert_eq!(shown.len(), expect.len(), "hidden 'iii' is not drawn: {shown:?}");
    for (got, want) in shown.iter().zip(expect) {
        assert!(got.0 == want.0 && (got.1 - want.1).abs() < 0.01 && (got.2 - want.2).abs() < 0.01, "{got:?} vs {want:?}");
    }
    // The anchored picture: box x_anchor..x_anchor+20, y 26..36 on line 0's baseline.
    let a = &anchors.placed[0].rect;
    assert!((a.x0_pt - x_anchor).abs() < 1e-9 && a.y0_pt == 26.0 && a.y1_pt == 36.0);
    let page = render_page(&out.bytes, 0, 1.0, &Limits::default(), &cancel).unwrap();
    let close = |p: [u8; 3], q: [u8; 3]| p.iter().zip(q).all(|(a, b)| a.abs_diff(b) <= 6);
    let red = page.pixel((x_anchor + 4.0) as u32, 31).unwrap();
    let blue = page.pixel((x_anchor + 15.0) as u32, 31).unwrap();
    let above = page.pixel((x_anchor + 4.0) as u32, 22).unwrap();
    assert!(close(red, [255, 0, 0]), "left half red {red:?}");
    assert!(close(blue, [0, 0, 255]), "right half blue {blue:?}");
    assert!(close(above, [255, 255, 255]), "nothing above the box {above:?}");
}

/// Chapter `n` for the book test: spread SPGS n of `pages` 300 x 400 pages p1.., one frame (layer
/// 10n, story n) on p1 at (20, 20), 200 x 100.
fn chapter_doc(n: u32, pages: usize) -> StudioDocument {
    let mut d = document();
    d.document_id = id("SDOC", n);
    d.page_spreads[0].page_spread_id = id("SPGS", n);
    d.page_spreads[0].pages = (1..=pages)
        .map(|i| StudioPage { page_id: format!("p{i}"), ..d.page_spreads[0].pages[0].clone() })
        .collect();
    let mut frame = d.layers[0].clone();
    frame.layer_id = id("SLYR", 10 * n);
    frame.parent = Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: id("SPGS", n) });
    frame.payload = Payload::Text { story_id: id("STXT", n) };
    frame.text_frame.as_mut().unwrap().page_id = "p1".into();
    d.layers = vec![frame];
    d
}

/// SMB-LAYOUT-UI slice 5: a two-chapter book (2 + 3 pages, chapter 2 starting on the next odd
/// page: 3) combined into one document and exported as ONE PDF: five pages in book order, each
/// chapter's "Hi" on the first page of its chapter at (20, 400 - 36), and /PageLabels written
/// from the book numbering.
#[test]
fn book_exports_one_pdf_with_continuous_page_labels() {
    use hsk_studio_interop_layout::layout::{BookChapter, combine_book};
    let cancel = CancellationToken::default();
    let (a, b) = (chapter_doc(1, 2), chapter_doc(2, 3));
    let mut book = StudioBook::new("book", "Book");
    book.add_chapter(&a.document_id).unwrap();
    book.add_chapter(&b.document_id).unwrap();
    book.repagination_option = StudioRepagination::ContinueOnNextOdd;
    validate_book(&book, Some(&[&a, &b]), &cancel).unwrap();
    let combined = combine_book(&book, &[BookChapter { document: &a, sections: &[] }, BookChapter { document: &b, sections: &[] }], &id("SDOC", 99)).unwrap();
    assert_eq!(combined.pages.iter().map(|p| p.label.as_str()).collect::<Vec<_>>(), ["1", "2", "3", "4", "5"]);
    let snapshot = validate_document(combined.document.clone(), Budget::default(), &cancel, &Everything).unwrap();
    let doc = snapshot.document();
    let flow = StudioLayoutFlowRecords::new(id("SDOC", 99));
    let mut composed = Vec::new();
    for n in [1u32, 2] {
        let mut st = story("Hi", &[(0, 2)]);
        st.story_id = id("STXT", n);
        let (paras, glyphs) = shaped("Hi", &[(0, 2)]);
        let req = ReflowRequest {
            document: doc,
            story: &st,
            resolved_text: None,
            paragraphs: &paras,
            obstacles: &[],
            composer: Composer::FirstFit,
            space: SpaceParams::default(),
            expected_revision: doc.revision,
            limits: hsk_studio_interop_layout::layout::Limits::default(),
        };
        let placement = reflow_flow(&req, &TypesetOptions::default(), &flow, &cancel).unwrap();
        composed.push((st, paras, glyphs, placement));
    }
    let stories: Vec<PdfStory<'_>> = composed
        .iter()
        .map(|(st, paras, glyphs, placement)| PdfStory { story: st, text: "Hi", placement, paragraphs: paras, glyphs, default_fill: Colour::DeviceGray(0.0) })
        .collect();
    let labels = page_labels_from_sections(&combined.sections, &combined.pages);
    let fonts = [PdfFont { bytes: INTER, face_index: 0, hyphen: None }];
    let picture = Picture::new();
    let input = PdfExportInput { snapshot: &snapshot, design: None, type_styles: None, stories: &stories, fonts: &fonts, assets: &picture, options: ExportOptions::default(), limits: Limits::default() };
    let out = export_pdf_with(&input, &PdfLongDoc { page_labels: &labels, ..PdfLongDoc::default() }, &cancel).unwrap();
    assert_eq!(out.pages, ["c0-p1", "c0-p2", "c1-p1", "c1-p2", "c1-p3"]);
    assert_eq!(out.glyphs_drawn, 4);
    assert!(find(&out.bytes, b"/PageLabels").is_some());
    // Glyphs: per page content stream in page order; "Hi" on pages 1 and 3 only.
    let widths = hsk_studio_interop_layout::press::font_advance_widths(INTER, 0).unwrap();
    let per_page: Vec<Vec<(u16, f64, f64)>> = streams(&out.bytes).iter().map(|s| common::pdf_scan::shown_runs(std::slice::from_ref(s), &widths)).filter(|g| !g.is_empty()).collect();
    assert_eq!(per_page.len(), 2);
    for g in &per_page {
        assert_eq!(g.iter().map(|x| x.0).collect::<Vec<_>>(), [161, 679]);
        assert!((g[0].1 - 20.0).abs() < 0.01 && (g[0].2 - 364.0).abs() < 0.01);
    }
    // Each page renders back (five pages, chapter 2's text on page index 2).
    let page2 = render_page(&out.bytes, 2, 1.0, &Limits::default(), &cancel).unwrap();
    let dark = (20u32..40).flat_map(|x| (20u32..40).map(move |y| (x, y))).filter(|&(x, y)| page2.pixel(x, y).is_some_and(|p| p[0] < 128)).count();
    assert!(dark > 0, "chapter 2 heading drawn on book page 3");
    assert!(render_page(&out.bytes, 5, 1.0, &Limits::default(), &cancel).is_err());
}
