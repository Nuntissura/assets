//! Native document -> PDF (SMB-LAYOUT-PDF-EXPORT slice 2). A Folio snapshot built here (two-page
//! facing spread with bleed, a justified text frame, a filled and stroked vector layer, a raster
//! layer, a text layer whose story is not supplied) is laid out by hsk-studio-layout and lowered
//! by `export_pdf`. Oracles are hand-computed from Inter's advances and the Folio geometry and are
//! checked against the written bytes: press's strict reader for the page boxes and an independent
//! scan of the decoded content streams (miniz_oxide inflate + line tokens) for glyph positions,
//! paths and 16-bit samples.
mod common;
use common::text_shaping::{FontFile, Seg, shape};
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_folio::*;
use hsk_studio_interop_layout::layout::{
    Alignment, Composer, JustifyParams, LayoutGlyph, Limits as ReflowLimits, ReflowRequest, ShapedParagraph, SourceSpan,
    SpaceParams, Spacing, TypesetOptions, reflow_typeset,
};
use hsk_studio_interop_layout::press::{Colour, ExportOptions, Limits, PdfDoc, render_page};
use hsk_studio_interop_layout::*;
use hsk_studio_nib::{FillRule as NibRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_pigment::{Grid, Rect, ResolvedTile};
use hsk_studio_prism::{ProfileInput, WORKING_PROFILE_LINEAR_SRGB, profile_hash};

/// Inter Regular (OFL, licence beside it). Glyph ids and advances (units of 2816 per em) read with
/// fontTools 4.62.1.
const INTER: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");
const SRGB: &[u8] = include_bytes!("../../hsk-studio-develop/tests/profiles/sRGB-v4.icc");
const UPM: f64 = 2816.0;
const GLYPHS: [(char, u16, f64); 12] = [
    ('H', 161, 2084.0),
    ('i', 679, 668.0),
    ('P', 374, 1788.0),
    ('D', 84, 2024.0),
    ('F', 137, 1652.0),
    ('w', 937, 2288.0),
    ('o', 775, 1680.0),
    ('r', 837, 1048.0),
    ('l', 723, 668.0),
    ('d', 594, 1748.0),
    ('e', 612, 1640.0),
    (' ', 1682, 792.0),
];
const SIZE: f64 = 20.0;
const TEXT: &str = "Hi PDF world";
/// Bleed: top/bottom 9, inside 3, outside 9. Page 1 is left-hand (inside = right edge).
const BLEED_IN: f64 = 3.0;
const BLEED_OUT: f64 = 9.0;

/// Lane SMB-LAYOUT-PDFX-VERAPDF: PDF/X-4 and PDF/A-2b identification over the sRGB output intent
/// (Folio colour is RGB).
fn archival() -> ExportOptions<'static> {
    use hsk_studio_interop_layout::press::{OutputIntent, OutputIntentSubtype, PdfX4};
    let intent = OutputIntent { icc: SRGB, components: 3, output_condition_identifier: "sRGB IEC61966-2.1", output_condition: None, registry: Some("http://www.color.org"), subtype: OutputIntentSubtype::GtsPdfx };
    let x4 = PdfX4 { created: "D:20261015000000Z", modified: "D:20261015000000Z", trapped: false };
    ExportOptions { version: "1.6", output_intent: Some(intent), info_title: Some("Studio layout corpus"), pdfx4: Some(x4), pdfa2b: true, ..ExportOptions::default() }
}

/// Writes `bytes` as `name` into `HSK_STUDIO_PDF_CORPUS_DIR` when it is set (external veraPDF run).
fn dump(name: &str, bytes: &[u8]) {
    if let Some(dir) = std::env::var_os("HSK_STUDIO_PDF_CORPUS_DIR") {
        std::fs::write(std::path::Path::new(&dir).join(name), bytes).unwrap();
    }
}

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
fn px(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Px }
}
fn edges(v: f64) -> LayoutEdges {
    LayoutEdges { top: len(v), right: len(v), bottom: len(v), left: len(v) }
}
fn page(key: &str, side: PageSide) -> StudioPage {
    StudioPage {
        page_id: key.into(),
        width: len(300.0),
        height: len(400.0),
        side,
        margins: edges(5.0),
        bleed: Some(PageBleed { top: len(9.0), bottom: len(9.0), inside_or_left: len(BLEED_IN), outside_or_right: len(BLEED_OUT) }),
    }
}
fn on_spread() -> Parent {
    Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: id("SPGS", 1) })
}
fn layer(n: u32, kind: &str, payload: Payload, frame: Option<StudioTextFrame>) -> StudioLayer {
    StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", n),
        name: format!("layer {n}"),
        parent: on_spread(),
        kind: kind.into(),
        payload,
        visible: true,
        appearance: None,
        text_frame: frame,
    }
}
fn frame(page: &str, x: f64, y: f64, w: f64, h: f64) -> StudioTextFrame {
    StudioTextFrame {
        page_id: page.into(),
        bounds: Bounds { x: len(x), y: len(y), width: len(w), height: len(h) },
        inset_spacing: edges(0.0),
        text_column_count: 1,
        text_column_gutter: len(0.0),
        first_baseline_offset: FirstBaseline::Fixed,
        minimum_first_baseline_offset: len(20.0),
        vertical_justification: VerticalJustification::Top,
        auto_sizing_type: AutoSizing::Off,
        previous_text_frame: None,
        next_text_frame: None,
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
fn tile_ref() -> TileRef {
    TileRef {
        object_key: "t0".into(),
        layer_id: id("SLYR", 3),
        column: 0,
        row: 0,
        width: px(2.0),
        height: px(2.0),
        format: "rgb_f32le".into(),
        colour_profile_id: id("SCPF", 1),
        schema_id: TileSchema::V1,
        content_digest: ContentDigest { algorithm: "sha256".into(), digest: "00".into() },
        artifact_manifest_id: "manifest-1".into(),
    }
}

/// Spread 1: page-1 (left-hand) and page-2 (right-hand), 300 x 400 pt each. Layers bottom first:
/// text frame (page 1), vector square (page 2), raster 2x2 (page 2), text of a story the export is
/// not given (page 1).
fn document() -> StudioDocument {
    let unassigned = || ProfileBinding::Unassigned(());
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision: 7,
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
            bounds: Bounds { x: len(0.0), y: len(0.0), width: len(600.0), height: len(400.0) },
            pages: vec![page("page-1", PageSide::LeftHand), page("page-2", PageSide::RightHand)],
        }],
        layers: vec![
            layer(1, "text", Payload::Text { story_id: id("STXT", 1) }, Some(frame("page-1", 20.0, 30.0, 100.0, 60.0))),
            layer(2, "vector", Payload::Vector { path_ids: vec![id("SVPT", 1)] }, None),
            layer(3, "raster", Payload::Raster { tiles: vec![tile_ref()] }, None),
            layer(4, "text", Payload::Text { story_id: id("STXT", 2) }, Some(frame("page-1", 20.0, 200.0, 100.0, 60.0))),
        ],
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

fn paint_style(n: u32, row: StudioAppearanceRow) -> StudioPaintStyle {
    StudioPaintStyle {
        schema_id: StyleRegistrySchema::V1,
        style_id: id("SSTY", n),
        kind: StyleKind::Paint,
        name: format!("style/{n}"),
        publishing: StudioPublishing {
            description: String::new(),
            description_markdown: String::new(),
            documentation_links: vec![],
            publish_key: format!("k{n}"),
            is_remote: false,
        },
        rows: vec![row],
    }
}
/// Fill: opaque red, no profile (DeviceRGB, reported). Stroke: blue bound to profile SCPF-1, 4 pt,
/// round cap, bevel join.
fn design() -> StudioDesignRecords {
    let colour = |r, g, b, p: ProfileBinding| StudioColor { r, g, b, colour_profile_id: p };
    let mut d = StudioDesignRecords::new(id("SDOC", 1));
    d.paint_styles.push(paint_style(
        1,
        StudioAppearanceRow::Fill {
            paint: StudioPaint::Solid { colour: colour(1.0, 0.0, 0.0, ProfileBinding::Unassigned(())) },
            opacity: 100.0,
            blend_mode: 2,
            visible: true,
        },
    ));
    d.paint_styles.push(paint_style(
        2,
        StudioAppearanceRow::Stroke {
            paint: StudioPaint::Solid { colour: colour(0.0, 0.0, 1.0, ProfileBinding::Assigned(id("SCPF", 1))) },
            stroke: StudioStrokeStyle {
                weight: len(4.0),
                cap: StrokeCap::Round,
                join: StrokeJoin::Bevel,
                align: StrokeAlign::Center,
                miter_limit: 4.0,
                dash: vec![],
                dash_offset: len(0.0),
            },
            opacity: 100.0,
            blend_mode: 2,
            visible: true,
        },
    ));
    let mut node: StudioNodeDesign = serde_json::from_value(serde_json::json!({
        "node": { "node_kind": "layer", "node_id": id("SLYR", 2) }
    }))
    .unwrap();
    node.style_refs.insert("fill_style_id".into(), id("SSTY", 1));
    node.style_refs.insert("stroke_style_id".into(), id("SSTY", 2));
    d.nodes.push(node);
    d
}

/// Linear-light samples: red 1.0, mid grey 0.5, black 0.0, white 1.0; alpha 1, 0.5, 0, 1.
fn planes() -> (Vec<u8>, Vec<u8>) {
    let rgb: [[f32; 3]; 4] = [[1.0, 0.0, 0.0], [0.5, 0.5, 0.5], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0]];
    let alpha: [f32; 4] = [1.0, 0.5, 0.0, 1.0];
    (rgb.iter().flatten().flat_map(|v| v.to_le_bytes()).collect(), alpha.iter().flat_map(|v| v.to_le_bytes()).collect())
}

struct Assets {
    tile: TileRef,
    profile_id: DomainId,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl PdfAssets for Assets {
    /// A 100 pt square at spread (350, 50): page 2 x 50..150 from its trim edge.
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        (path_id == id("SVPT", 1)).then(|| OwnedPath {
            elements: vec![
                PathElement::MoveTo(PathPoint::new(350.0, 50.0)),
                PathElement::LineTo(PathPoint::new(450.0, 50.0)),
                PathElement::QuadTo(PathPoint::new(450.0, 150.0), PathPoint::new(450.0, 150.0)),
                PathElement::LineTo(PathPoint::new(350.0, 150.0)),
                PathElement::Close,
            ],
            fill_rule: NibRule::EvenOdd,
        })
    }
    /// 2x2 tile at spread (400, 300), 10 pt per pixel.
    fn raster(&self, layer_id: &str) -> Option<PlacedRaster<'_>> {
        (layer_id == id("SLYR", 3)).then(|| PlacedRaster {
            grid: Grid { origin_x: 0, origin_y: 0, tile_width: 2, tile_height: 2 },
            tiles: vec![ResolvedTile {
                tile_ref: &self.tile,
                bounds: Rect { x: 0, y: 0, width: 2, height: 2 },
                sample_format: "rgb_f32le",
                colour_bytes: &self.colour,
                colour_stride_bytes: 24,
                colour_sha256: profile_hash(&self.colour),
                alpha_bytes: &self.alpha,
                alpha_stride_bytes: 8,
                alpha_sha256: profile_hash(&self.alpha),
                transfer: "linear_light",
                alpha_association: "straight",
                profile: ProfileInput { profile_id: &self.profile_id, bytes: WORKING_PROFILE_LINEAR_SRGB, expected_sha256: profile_hash(WORKING_PROFILE_LINEAR_SRGB) },
            }],
            origin_pt: [400.0, 300.0],
            pt_per_px: 10.0,
        })
    }
    fn profile(&self, profile_id: &str) -> Option<&[u8]> {
        (profile_id == id("SCPF", 1)).then_some(SRGB)
    }
}

fn glyph(c: char) -> (u16, f64) {
    let (_, g, adv) = GLYPHS.iter().copied().find(|(ch, ..)| *ch == c).unwrap();
    (g, adv / UPM * SIZE)
}

struct Built {
    snapshot: Snapshot,
    story: StudioTextStory,
    paragraphs: Vec<ShapedParagraph>,
    glyphs: Vec<Vec<PdfGlyph>>,
}
fn build() -> Built {
    let cancel = CancellationToken::default();
    let snapshot = validate_document(document(), Budget::default(), &cancel, &Everything).unwrap();
    let st = story(1, TEXT);
    let glyphs: Vec<LayoutGlyph> = TEXT
        .char_indices()
        .map(|(at, c)| LayoutGlyph { cluster: SourceSpan { start: at as u64, end: (at + 1) as u64 }, advance_pt: glyph(c).1 })
        .collect();
    let paragraphs = vec![ShapedParagraph::latin_ltr(SourceSpan { start: 0, end: TEXT.len() as u64 }, glyphs, 24.0, 16.0)];
    let ids = vec![TEXT.chars().map(|c| PdfGlyph { glyph_id: glyph(c).0, font: 0, size_pt: SIZE, x_offset_pt: 0.0, y_offset_pt: 0.0 }).collect()];
    Built { snapshot, story: st, paragraphs, glyphs: ids }
}

/// Justified, word spacing allowed up to 20x so the first line fills its 100 pt slot exactly.
fn placement(b: &Built) -> hsk_studio_interop_layout::layout::LayoutProposal {
    let doc = b.snapshot.document();
    let opts = TypesetOptions {
        justify: Some(JustifyParams { word_spacing: Spacing { min: 0.8, desired: 1.0, max: 20.0 }, ..JustifyParams::default() }),
        alignment: Alignment::Justified,
        ..TypesetOptions::default()
    };
    reflow_typeset(
        &ReflowRequest {
            document: doc,
            story: &b.story,
            resolved_text: None,
            paragraphs: &b.paragraphs,
            obstacles: &[],
            composer: Composer::FirstFit,
            space: SpaceParams::default(),
            expected_revision: doc.revision,
            limits: ReflowLimits::default(),
        },
        &opts,
        &CancellationToken::default(),
    )
    .unwrap()
}

fn assets() -> Assets {
    let (colour, alpha) = planes();
    Assets { tile: tile_ref(), profile_id: DomainId::parse(&id("SCPF", 1)).unwrap(), colour, alpha }
}

/// Every Flate stream of the file, decoded (independent of press's reader).
fn streams(pdf: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut at = 0;
    // A stream body follows its dictionary's `>>` and ends at the `\nendstream` the writer appends.
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
/// `(gid, x, y)` of every drawn glyph of the content streams (text matrices, `Tj`, `TJ` runs with
/// Inter's program advances; tests/common/pdf_scan.rs).
fn shown(streams: &[Vec<u8>]) -> Vec<(u16, f64, f64)> {
    common::pdf_scan::shown_runs(streams, &hsk_studio_interop_layout::press::font_advance_widths(INTER, 0).unwrap())
}

#[test]
fn snapshot_lowers_to_pdf() {
    let cancel = CancellationToken::default();
    let b = build();
    let place = placement(&b);
    let lines = &place.frames[0].lines;
    assert_eq!(lines.len(), 2, "{lines:?}");
    let assets = assets();
    let design = design();
    let stories = [PdfStory {
        story: &b.story,
        text: TEXT,
        placement: &place,
        paragraphs: &b.paragraphs,
        glyphs: &b.glyphs,
        default_fill: Colour::DeviceGray(0.0),
    }];
    let fonts = [PdfFont { bytes: INTER, face_index: 0, hyphen: None }];
    let out = export_pdf(
        &PdfExportInput {
            snapshot: &b.snapshot,
            design: Some(&design),
            type_styles: None,
            stories: &stories,
            fonts: &fonts,
            assets: &assets,
            options: ExportOptions::default(),
            limits: Limits::default(),
        },
        &cancel,
    )
    .unwrap();
    // Pages: trim 300 x 400 plus bleed; page 1 left-hand (outside = left), page 2 right-hand.
    assert_eq!(out.pages, ["page-1", "page-2"]);
    let doc = PdfDoc::open(&out.bytes, &Limits::default(), &cancel).unwrap();
    let pages = doc.proposal(&cancel).unwrap().pages;
    assert_eq!(pages[0].media_box, Some([0.0, 0.0, 312.0, 418.0]));
    assert_eq!(pages[1].media_box, Some([0.0, 0.0, 312.0, 418.0]));
    assert_eq!((out.receipt.pages, out.receipt.fonts_embedded, out.receipt.images), (2, 1, 1));
    let decoded = streams(&out.bytes);
    // Text: frame at (20, 30) on page 1, baseline 20 below the frame top; page 1's left bleed is 9.
    // PDF x = 9 + 20 + pen, y = 9 + 400 - 50 = 359 (line 1) and 335 (line 2, line height 24).
    let g = shown(&decoded);
    // Every glyph of both lines is drawn (spaces included, so text extraction keeps them).
    assert_eq!(g.len(), TEXT.chars().count(), "{g:?}");
    let (h, _) = glyph('H');
    assert_eq!(g[0], (h, 29.0, 359.0));
    // Line 1 "Hi PDF" justified to the 100 pt slot: F ends exactly at 29 + 100.
    let f = g.iter().find(|(id, ..)| *id == glyph('F').0).unwrap();
    assert!((f.1 + glyph('F').1 - 129.0).abs() < 1e-6, "{f:?}");
    // Line 2 "world", last line, not justified: w at 29, then the advances.
    let w = g.iter().position(|(id, ..)| *id == glyph('w').0).unwrap();
    assert_eq!((g[w].1, g[w].2), (29.0, 335.0));
    assert!((g[w + 1].1 - (29.0 + glyph('w').1)).abs() < 1e-9);
    assert_eq!(out.glyphs_drawn, g.len() as u64);
    // Vector square on page 2: left bleed 3 (inside), spread x 350 -> 53, spread y 50 -> 359.
    let content: String = decoded.iter().map(|s| String::from_utf8_lossy(s).into_owned()).collect();
    assert!(content.contains("1.0 0.0 0.0 rg\n53.0 359.0 m\n153.0 359.0 l\n"), "fill");
    assert!(content.contains("f*\n"), "even-odd fill");
    assert!(content.contains("4.0 w\n1 J\n2 j\n"), "stroke style");
    assert!(content.contains("/CS0 CS\n0.0 0.0 1.0 SC\n"), "ICC stroke colour");
    assert_eq!(out.paths_drawn, 2);
    // Raster: 2x2 at spread (400, 300), 10 pt/px -> page 2 x 103, bottom y 9 + 400 - 320 = 89.
    assert!(content.contains("20.0 0.0 0.0 20.0 103.0 89.0 cm\n"), "image placement");
    assert_eq!(out.tiles_drawn, 1);
    let (q1, q5) = (65535u16.to_be_bytes(), 32768u16.to_be_bytes());
    let samples = [q1, [0, 0], [0, 0], q5, q5, q5, [0, 0], [0, 0], [0, 0], q1, q1, q1].concat();
    let alpha = [q1, q5, [0, 0], q1].concat();
    assert!(decoded.contains(&samples), "16-bit linear samples");
    assert!(decoded.contains(&alpha), "16-bit alpha");
    assert!(decoded.iter().any(|s| s.as_slice() == WORKING_PROFILE_LINEAR_SRGB), "tile profile embedded");
    // ToUnicode maps the drawn ids back to the story text.
    assert!(content.contains(&format!("<{:04X}> <0048>", h)), "ToUnicode H");
    // Losses: the untagged red fill and the story that was not supplied.
    let codes: Vec<&str> = out.loss.iter().map(|l| l.code).collect();
    // No graph outputs: document order, reported.
    assert_eq!(codes, ["paint_colour_untagged", "story_not_supplied", "composition_graph_absent"], "{:?}", out.loss);
    // Render-back (press render_page: hayro-interpret into press's paint device) at 1 px per pt;
    // device y = 418 - PDF y.
    let page1 = render_page(&out.bytes, 0, 1.0, &Limits::default(), &cancel).unwrap();
    assert_eq!((page1.width, page1.height, page1.font_warnings), (312, 418, 0));
    let ink = (43..60).flat_map(|y| (29..129).map(move |x| (x, y))).filter(|(x, y)| page1.pixel(*x, *y) != Some([255, 255, 255])).count();
    assert!(ink > 50, "glyph ink {ink}");
    assert_eq!(page1.pixel(200, 300), Some([255, 255, 255]));
    let page2 = render_page(&out.bytes, 1, 1.0, &Limits::default(), &cancel).unwrap();
    assert_eq!(page2.pixel(103, 109), Some([255, 0, 0]), "square fill");
    let edge = page2.pixel(53, 109).unwrap();
    assert!(edge[2] > 200 && edge[0] < 60, "ICC blue stroke {edge:?}");
    let px = |x, y| page2.pixel(x, y).unwrap();
    let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(p, q)| p.abs_diff(q) <= 6);
    assert!(near(px(108, 314), [255, 0, 0]), "linear red {:?}", px(108, 314));
    assert_eq!(px(108, 324), [255, 255, 255], "alpha 0");
    assert!(near(px(118, 324), [255, 255, 255]), "linear white {:?}", px(118, 324));
    let grey = px(118, 314);
    assert!(grey[0].abs_diff(grey[1]) <= 3 && (150..=245).contains(&grey[0]), "half-alpha linear grey {grey:?}");
    // The same document with the archival identification, for the veraPDF corpus.
    let input = PdfExportInput { snapshot: &b.snapshot, design: Some(&design), type_styles: None, stories: &stories, fonts: &fonts, assets: &assets, options: archival(), limits: Limits::default() };
    let x = export_pdf(&input, &cancel).unwrap();
    assert_eq!(x.loss.len(), out.loss.len());
    dump("layout-snapshot.pdf", &x.bytes);
}

#[test]
fn misaligned_stale_and_refused_inputs() {
    let cancel = CancellationToken::default();
    let b = build();
    let place = placement(&b);
    let assets = assets();
    let fonts = [PdfFont { bytes: INTER, face_index: 0, hyphen: None }, PdfFont { bytes: b"not a font", face_index: 0, hyphen: None }];
    let run = |glyphs: &[Vec<PdfGlyph>], place: &hsk_studio_interop_layout::layout::LayoutProposal, token: &CancellationToken| {
        let stories = [PdfStory { story: &b.story, text: TEXT, placement: place, paragraphs: &b.paragraphs, glyphs, default_fill: Colour::DeviceGray(0.0) }];
        export_pdf(
            &PdfExportInput {
                snapshot: &b.snapshot,
                design: None,
                type_styles: None,
                stories: &stories,
                fonts: &fonts,
                assets: &assets,
                options: ExportOptions::default(),
                limits: Limits::default(),
            },
            token,
        )
    };
    let mut short = b.glyphs.clone();
    short[0].pop();
    assert_eq!(run(&short, &place, &cancel).unwrap_err(), PdfExportError::InvalidInput("glyphs_misaligned"));
    let mut bad_font = b.glyphs.clone();
    bad_font[0][0].font = 9;
    assert_eq!(run(&bad_font, &place, &cancel).unwrap_err(), PdfExportError::InvalidInput("font_index"));
    let mut nan = b.glyphs.clone();
    nan[0][1].size_pt = f64::NAN;
    assert_eq!(run(&nan, &place, &cancel).unwrap_err(), PdfExportError::InvalidInput("glyph_metrics"));
    let mut stale = place.clone();
    stale.base_revision = 6;
    assert_eq!(run(&b.glyphs, &stale, &cancel).unwrap_err(), PdfExportError::StaleRevision { expected: 6, current: 7 });
    let mut range = place.clone();
    range.frames[0].lines[0].glyph_range = 0..999;
    assert_eq!(run(&b.glyphs, &range, &cancel).unwrap_err(), PdfExportError::InvalidInput("line_glyph_range"));
    // A font press refuses is a loss; its glyphs are not drawn and are counted.
    let mut second = b.glyphs.clone();
    for g in &mut second[0][..2] {
        g.font = 1;
    }
    let out = run(&second, &place, &cancel).unwrap();
    let codes: Vec<(&str, u32)> = out.loss.iter().map(|l| (l.code, l.count)).collect();
    assert!(codes.contains(&("font_unreadable", 1)), "{codes:?}");
    assert!(codes.contains(&("glyph_font_not_embedded", 2)), "{codes:?}");
    // Without design records the vector layer has no paint rows: nothing drawn, nothing to report.
    assert_eq!(out.paths_drawn, 0);
    let tripped = CancellationToken::default();
    tripped.cancel();
    assert_eq!(run(&b.glyphs, &place, &tripped).unwrap_err(), PdfExportError::Canceled);
    // The DESCRIPTOR names the entry point and stays valid JSON.
    let d: serde_json::Value = serde_json::from_str(hsk_studio_interop_layout::DESCRIPTOR).unwrap();
    assert!(d["pdf_export"]["entry"].as_str().unwrap().starts_with("export_pdf("));
}

fn vector_in_group(n: u32, path: u32) -> StudioLayer {
    let mut l = layer(n, "vector", Payload::Vector { path_ids: vec![id("SVPT", path)] }, None);
    l.parent = Parent::Node(NodeRef { node_kind: NodeKind::Layer, node_id: id("SLYR", 10) });
    l
}

/// Group 10 (on the spread) composites vector 12 (ordinal 0, bottom) under vector 11 (ordinal 1);
/// the document lists 11 before 12, and raster 3 is outside the graph.
fn graph_document() -> StudioDocument {
    let mut d = document();
    let src = |n: u32| -> Operation {
        serde_json::from_value(serde_json::json!({"layer_id": id("SLYR", n), "operation_key": "source", "operation_kind": "source", "inputs": [],
            "outputs": [{"port_key": "out", "value_type": "image", "cardinality": "one", "semantic": "source"}]})).unwrap()
    };
    let edge = |n: u32, ordinal: u64| -> Edge {
        serde_json::from_value(serde_json::json!({"source": {"layer_id": id("SLYR", n), "operation_key": "source", "port_key": "out"},
            "target": {"layer_id": id("SLYR", 10), "operation_key": "composite", "port_key": "inputs"}, "dependency_kind": "data", "input_ordinal": ordinal})).unwrap()
    };
    d.layers = vec![
        layer(10, "group", Payload::Group, None),
        vector_in_group(11, 11),
        vector_in_group(12, 12),
        layer(3, "raster", Payload::Raster { tiles: vec![tile_ref()] }, None),
    ];
    d.graph = serde_json::from_value(serde_json::json!({"schema_id": "hsk.studio.layer_graph@1",
        "operations": [
            {"layer_id": id("SLYR", 10), "operation_key": "composite", "operation_kind": "composite",
             "inputs": [{"port_key": "inputs", "value_type": "image", "cardinality": "many", "semantic": "ordered_composite"}],
             "outputs": [{"port_key": "out", "value_type": "image", "cardinality": "one", "semantic": "composite_result"}]}],
        "edges": [], "outputs": [{"layer_id": id("SLYR", 10), "operation_key": "composite", "port_key": "out"}]})).unwrap();
    d.graph.operations.push(src(11));
    d.graph.operations.push(src(12));
    d.graph.edges = vec![edge(12, 0), edge(11, 1)];
    d
}

struct GraphAssets;
impl PdfAssets for GraphAssets {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        let x = if path_id == id("SVPT", 11) { 350.0 } else if path_id == id("SVPT", 12) { 360.0 } else { return None };
        Some(OwnedPath {
            elements: vec![
                PathElement::MoveTo(PathPoint::new(x, 50.0)),
                PathElement::LineTo(PathPoint::new(x + 50.0, 50.0)),
                PathElement::LineTo(PathPoint::new(x + 50.0, 100.0)),
                PathElement::Close,
            ],
            fill_rule: NibRule::NonZero,
        })
    }
    fn raster(&self, _: &str) -> Option<PlacedRaster<'_>> {
        None
    }
    fn profile(&self, _: &str) -> Option<&[u8]> {
        None
    }
}

#[test]
fn layer_graph_decides_the_paint_order() {
    let cancel = CancellationToken::default();
    let snapshot = validate_document(graph_document(), Budget::default(), &cancel, &Everything).unwrap();
    let mut design = StudioDesignRecords::new(id("SDOC", 1));
    let fill = |n: u32, rgb: [f64; 3]| {
        paint_style(n, StudioAppearanceRow::Fill {
            paint: StudioPaint::Solid { colour: StudioColor { r: rgb[0], g: rgb[1], b: rgb[2], colour_profile_id: ProfileBinding::Unassigned(()) } },
            opacity: 100.0,
            blend_mode: 2,
            visible: true,
        })
    };
    design.paint_styles.push(fill(11, [1.0, 0.0, 0.0]));
    design.paint_styles.push(fill(12, [0.0, 0.0, 1.0]));
    for n in [11u32, 12] {
        let mut node: StudioNodeDesign = serde_json::from_value(serde_json::json!({"node": {"node_kind": "layer", "node_id": id("SLYR", n)}})).unwrap();
        node.style_refs.insert("fill_style_id".into(), id("SSTY", n));
        design.nodes.push(node);
    }
    let out = export_pdf(
        &PdfExportInput {
            snapshot: &snapshot,
            design: Some(&design),
            type_styles: None,
            stories: &[],
            fonts: &[],
            assets: &GraphAssets,
            options: ExportOptions::default(),
            limits: Limits::default(),
        },
        &cancel,
    )
    .unwrap();
    let content: String = streams(&out.bytes).iter().map(|s| String::from_utf8_lossy(s).into_owned()).collect();
    let (blue, red) = (content.find("0.0 0.0 1.0 rg").unwrap(), content.find("1.0 0.0 0.0 rg").unwrap());
    assert!(blue < red, "ordinal 0 (blue, listed second) is painted first");
    assert_eq!(out.paths_drawn, 2);
    let codes: Vec<&str> = out.loss.iter().map(|l| l.code).collect();
    assert!(codes.contains(&"layer_not_in_composition_graph"), "{codes:?}");
    assert!(!codes.contains(&"composition_graph_absent"), "{codes:?}");
}

fn type_style(n: u32, scope: TypeStyleScope, fill: StudioRgba) -> StudioTypeStyle {
    StudioTypeStyle {
        schema_id: TypeStyleSchema::V1,
        style_id: id("STYS", n),
        scope,
        name: format!("style/{n}"),
        based_on: None,
        next_style: None,
        font_family: None,
        font_style: None,
        font_size: None,
        fill: Some(fill),
        line_height: None,
        letter_spacing: None,
        align: None,
    }
}

/// Type shapes the story (Inter, 20 pt, through the TextEngine double), layout composes the
/// flattened paragraph, `pdf_glyphs_from_type` supplies the ids; the paragraph style's fill (green,
/// bound to profile SCPF-1) colours the text and the character style on "PDF" (red, half alpha,
/// unassigned) colours those three glyphs.
#[test]
fn type_shaped_story_takes_its_fill_from_folio_styles() {
    let cancel = CancellationToken::default();
    let b = build();
    let mut st = b.story.clone();
    st.paragraphs[0].style_ref = Some(id("STYS", 1));
    st.character_ranges = vec![
        StoryRange { start: 0, end: 3, style_ref: None, applied_language: Some("en-US".into()) },
        StoryRange { start: 3, end: 6, style_ref: Some(id("STYS", 2)), applied_language: Some("en-US".into()) },
        StoryRange { start: 6, end: TEXT.len() as u64, style_ref: None, applied_language: Some("en-US".into()) },
    ];
    let mut styles = StudioTypeStyleRecords::new(id("SDOC", 1));
    styles.styles = vec![
        type_style(1, TypeStyleScope::Paragraph, StudioRgba { r: 0.0, g: 0.5, b: 0.0, a: 1.0, colour_profile_id: ProfileBinding::Assigned(id("SCPF", 1)) }),
        type_style(2, TypeStyleScope::Character, StudioRgba { r: 1.0, g: 0.0, b: 0.0, a: 0.5, colour_profile_id: ProfileBinding::Unassigned(()) }),
    ];
    let files = [FontFile::inter()];
    let fonts = [PdfFont { bytes: &files[0].bytes, face_index: 0, hyphen: None }];
    let segs = [Seg { start: 0, end: TEXT.len() as u64, font: 0, script: *b"Latn", language: "en", axes: vec![] }];
    let (paragraphs, glyphs) = shape(&files, TEXT, &segs, false, SIZE, |prepared| {
        let p = &prepared.paragraphs()[0];
        (vec![ShapedParagraph::from_type(p, 24.0, 16.0)], vec![pdf_glyphs_from_type(p, &fonts).unwrap()])
    });
    assert_eq!(glyphs[0].len(), paragraphs[0].glyphs.len());
    assert_eq!(glyphs[0][0], PdfGlyph { glyph_id: glyph('H').0, font: 0, size_pt: SIZE, x_offset_pt: 0.0, y_offset_pt: 0.0 });
    let snapshot = &b.snapshot;
    let doc = snapshot.document();
    let place = reflow_typeset(
        &ReflowRequest {
            document: doc,
            story: &st,
            resolved_text: None,
            paragraphs: &paragraphs,
            obstacles: &[],
            composer: Composer::FirstFit,
            space: SpaceParams::default(),
            expected_revision: doc.revision,
            limits: ReflowLimits::default(),
        },
        &TypesetOptions::default(),
        &cancel,
    )
    .unwrap();
    let stories = [PdfStory { story: &st, text: TEXT, placement: &place, paragraphs: &paragraphs, glyphs: &glyphs, default_fill: Colour::DeviceGray(0.0) }];
    let assets = assets();
    let out = export_pdf(
        &PdfExportInput {
            snapshot,
            design: None,
            type_styles: Some(&styles),
            stories: &stories,
            fonts: &fonts,
            assets: &assets,
            options: ExportOptions::default(),
            limits: Limits::default(),
        },
        &cancel,
    )
    .unwrap();
    let decoded = streams(&out.bytes);
    let content: String = decoded.iter().map(|s| String::from_utf8_lossy(s).into_owned()).collect();
    // Three runs in order: green (ICC) "Hi ", red "PDF", green " world" (layout places them on one or
    // two lines; each colour change starts a new BT block).
    let green = "/CS0 cs\n0.0 0.5 0.0 sc\nBT\n";
    let red = "1.0 0.0 0.0 rg\nBT\n";
    let (g1, r1) = (content.find(green).unwrap(), content.find(red).unwrap());
    let g2 = content[r1..].find(green).map(|i| i + r1).unwrap();
    assert!(g1 < r1 && r1 < g2, "fills follow the character ranges");
    let shown_ids: Vec<u16> = shown(&decoded).iter().map(|g| g.0).collect();
    assert_eq!(shown_ids, glyphs[0].iter().map(|g| g.glyph_id).collect::<Vec<_>>(), "Type's ids, in order");
    let codes: Vec<&str> = out.loss.iter().map(|l| l.code).collect();
    assert!(codes.contains(&"paint_colour_untagged") && !codes.contains(&"text_fill_alpha_ignored"), "{codes:?}");
    // The half-alpha red run is drawn through an /ExtGState with /ca 0.5.
    assert!(content.contains("q\n/GS0 gs\n1.0 0.0 0.0 rg\nBT\n"), "alpha run");
    assert!(find(&out.bytes, b"/ca 0.5 /CA 1 /BM /Normal").is_some(), "ExtGState ca");
    // A run whose font is not supplied is refused by the adapter.
    let other = [PdfFont { bytes: INTER, face_index: 1, hyphen: None }];
    let refused = shape(&files, TEXT, &segs, false, SIZE, |prepared| pdf_glyphs_from_type(&prepared.paragraphs()[0], &other));
    assert_eq!(refused, Err(PdfExportError::InvalidInput("font_not_supplied")));
}

struct MaskAssets {
    coverage: Vec<u8>,
}
impl PdfAssets for MaskAssets {
    fn path(&self, path_id: &str) -> Option<OwnedPath> {
        GraphAssets.path(path_id)
    }
    fn raster(&self, _: &str) -> Option<PlacedRaster<'_>> {
        None
    }
    fn profile(&self, _: &str) -> Option<&[u8]> {
        None
    }
    /// 2x1 coverage (255, 0) over spread (350, 50) at 25 pt per pixel.
    fn mask(&self, layer_id: &str) -> Option<PlacedMask<'_>> {
        (layer_id == id("SLYR", 10)).then(|| PlacedMask {
            mask: hsk_studio_pigment::Mask { bounds: Rect { x: 0, y: 0, width: 2, height: 1 }, sample_format: "coverage_u8", bytes: &self.coverage, stride_bytes: 2, sha256: profile_hash(&self.coverage) },
            origin_pt: [350.0, 50.0],
            pt_per_px: 25.0,
        })
    }
}

/// Item 6 through Folio appearance: group 10 at 50 percent opacity with Multiply becomes a
/// transparency group (form XObject + /ExtGState); vector 11's Vivid Light has no PDF mode (drawn
/// Normal, reported) and its 50 percent fill row goes through /ca; an enabled mask becomes a
/// luminosity soft mask. Render-back without the mask: blue (12 only) at 50 percent under
/// Multiply over white, red (11 only) at 50 percent.
#[test]
fn folio_appearance_becomes_transparency_groups() {
    let cancel = CancellationToken::default();
    let appearance = |opacity: f64, blend: u16, mask: bool| LayerAppearance {
        opacity,
        fill_opacity: 100.0,
        blend_mode: blend,
        clipped: false,
        isolation: Isolation::Isolated,
        mask: mask.then(|| StudioMask { tiles: vec![], enabled: true, linked: true }),
    };
    let run = |mask: bool| {
        let mut d = graph_document();
        d.layers[0].appearance = Some(appearance(50.0, 5, mask));
        d.layers[1].appearance = Some(appearance(100.0, 15, false));
        let snapshot = validate_document(d, Budget::default(), &cancel, &Everything).unwrap();
        let mut design = StudioDesignRecords::new(id("SDOC", 1));
        let fill = |n: u32, rgb: [f64; 3], opacity: f64| {
            paint_style(n, StudioAppearanceRow::Fill {
                paint: StudioPaint::Solid { colour: StudioColor { r: rgb[0], g: rgb[1], b: rgb[2], colour_profile_id: ProfileBinding::Unassigned(()) } },
                opacity,
                blend_mode: 2,
                visible: true,
            })
        };
        design.paint_styles.push(fill(11, [1.0, 0.0, 0.0], 50.0));
        design.paint_styles.push(fill(12, [0.0, 0.0, 1.0], 100.0));
        for n in [11u32, 12] {
            let mut node: StudioNodeDesign = serde_json::from_value(serde_json::json!({"node": {"node_kind": "layer", "node_id": id("SLYR", n)}})).unwrap();
            node.style_refs.insert("fill_style_id".into(), id("SSTY", n));
            design.nodes.push(node);
        }
        let assets = MaskAssets { coverage: vec![255, 0] };
        export_pdf(
            &PdfExportInput {
                snapshot: &snapshot,
                design: Some(&design),
                type_styles: None,
                stories: &[],
                fonts: &[],
                assets: &assets,
                options: archival(),
                limits: Limits::default(),
            },
            &cancel,
        )
        .unwrap()
    };
    let masked = run(true);
    dump("layout-appearance-mask.pdf", &masked.bytes);
    let codes: Vec<&str> = masked.loss.iter().map(|l| l.code).collect();
    assert!(codes.contains(&"blend_mode_not_in_pdf"), "{codes:?}");
    assert!(!codes.contains(&"layer_mask_unresolved"), "{codes:?}");
    assert!(find(&masked.bytes, b"/ca 0.5 /CA 0.5 /BM /Multiply /SMask").is_some(), "group ExtGState");
    assert!(find(&masked.bytes, b"/S /Luminosity").is_some(), "soft mask");
    assert!(find(&masked.bytes, b"/S /Transparency /I true").is_some(), "transparency group");
    assert!(streams(&masked.bytes).contains(&vec![255u8, 0]), "mask coverage samples");
    let page2 = render_page(&masked.bytes, 1, 1.0, &Limits::default(), &cancel).unwrap();
    // The mask (coverage 255 | 0 over spread x 350..375 | 375..400, y 50..75; 0 outside) keeps
    // the red-only pixel (spread 355, 52) and removes the blue-only pixel (spread 405, 55).
    let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 4);
    assert!(near(page2.pixel(58, 61).unwrap(), [255, 191, 191]), "{:?}", page2.pixel(58, 61));
    assert_eq!(page2.pixel(108, 64), Some([255, 255, 255]), "outside the mask");
    assert!(page2.notes.is_empty(), "{:?}", page2.notes);
    // Without the mask the render-back shows the group opacity, the Multiply and the row alpha.
    let plain = run(false);
    let r = render_page(&plain.bytes, 1, 1.0, &Limits::default(), &cancel).unwrap();
    let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 4);
    // Spread (405, 55) -> page 2 (108, 354) -> device (108, 64): vector 12 only.
    assert!(near(r.pixel(108, 64).unwrap(), [128, 128, 255]), "{:?}", r.pixel(108, 64));
    // Spread (355, 52) -> device (58, 61): vector 11 only, 50 percent row inside the 50 percent group.
    assert!(near(r.pixel(58, 61).unwrap(), [255, 191, 191]), "{:?}", r.pixel(58, 61));
}

/// Lane SMB-LAYOUT-PDF-EXPORT-3 through Folio: vector 11 is clipped to its base (vector 12, ordinal
/// 0) and filled with a black -> white linear gradient over spread x 350..400; the document is
/// written with the PDF/X-4 identification and passes the crate's self-check (not_proven).
#[test]
fn folio_clipping_gradient_and_pdfx4() {
    let cancel = CancellationToken::default();
    let mut d = graph_document();
    d.layers[1].appearance = Some(LayerAppearance { opacity: 100.0, fill_opacity: 100.0, blend_mode: 2, clipped: true, isolation: Isolation::Isolated, mask: None });
    let snapshot = validate_document(d, Budget::default(), &cancel, &Everything).unwrap();
    let mut design = StudioDesignRecords::new(id("SDOC", 1));
    let stop = |position: f64, v: f64| StudioGradientStop { position, midpoint: None, opacity: 100.0, colour: StudioColor { r: v, g: v, b: v, colour_profile_id: ProfileBinding::Assigned(id("SCPF", 1)) } };
    design.paint_styles.push(paint_style(11, StudioAppearanceRow::Fill {
        paint: StudioPaint::Gradient {
            gradient: StudioGradient {
                geometry: StudioGradientGeometry::Linear { x1: 350.0, y1: 0.0, x2: 400.0, y2: 0.0 },
                transform: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                spread: GradientSpread::Pad,
                interpolation: GradientInterpolation::Classic,
                dither: false,
                stops: vec![stop(0.0, 0.0), stop(100.0, 1.0)],
            },
        },
        opacity: 100.0,
        blend_mode: 2,
        visible: true,
    }));
    design.paint_styles.push(paint_style(12, StudioAppearanceRow::Fill {
        paint: StudioPaint::Solid { colour: StudioColor { r: 0.0, g: 0.0, b: 1.0, colour_profile_id: ProfileBinding::Assigned(id("SCPF", 1)) } },
        opacity: 100.0,
        blend_mode: 2,
        visible: true,
    }));
    for n in [11u32, 12] {
        let mut node: StudioNodeDesign = serde_json::from_value(serde_json::json!({"node": {"node_kind": "layer", "node_id": id("SLYR", n)}})).unwrap();
        node.style_refs.insert("fill_style_id".into(), id("SSTY", n));
        design.nodes.push(node);
    }
    struct Assets;
    impl PdfAssets for Assets {
        fn path(&self, path_id: &str) -> Option<OwnedPath> {
            GraphAssets.path(path_id)
        }
        fn raster(&self, _: &str) -> Option<PlacedRaster<'_>> {
            None
        }
        fn profile(&self, profile_id: &str) -> Option<&[u8]> {
            (profile_id == id("SCPF", 1)).then_some(SRGB)
        }
    }
    let x4 = hsk_studio_interop_layout::press::PdfX4 { created: "D:20261015000000Z", modified: "D:20261015000000Z", trapped: false };
    let intent = hsk_studio_interop_layout::press::OutputIntent {
        icc: SRGB,
        components: 3,
        output_condition_identifier: "sRGB",
        output_condition: None,
        registry: Some("http://www.color.org"),
        subtype: hsk_studio_interop_layout::press::OutputIntentSubtype::GtsPdfx,
    };
    let out = export_pdf(
        &PdfExportInput {
            snapshot: &snapshot,
            design: Some(&design),
            type_styles: None,
            stories: &[],
            fonts: &[],
            assets: &Assets,
            options: ExportOptions { output_intent: Some(intent), info_title: Some("folio x4"), pdfx4: Some(x4), pdfa2b: true, ..ExportOptions::default() },
            limits: Limits::default(),
        },
        &cancel,
    )
    .unwrap();
    let codes: Vec<&str> = out.loss.iter().map(|l| l.code).collect();
    assert!(!codes.iter().any(|c| c.starts_with("clip_") || c.starts_with("gradient_") || *c == "paint_gradient"), "{codes:?}");
    dump("layout-clip-gradient.pdf", &out.bytes);
    // veraPDF 1.30.3 PDF/A-2b passed these exact bytes (raw run studio-team-dev/r31-findings/verapdf,
    // committed in its press::normalize_verapdf_report form: item name = file name, no machine
    // path); only that report moves the receipt off not_proven.
    assert_eq!(out.receipt.sha256_hex, "3405693a3cf44a39e97bbcaae250a3ab0ccbbce8466f82f3e84e88e8524934f0", "export changed: re-run veraPDF, refresh the fixture");
    assert_eq!(out.receipt.conformance, "not_proven");
    let report = include_bytes!("fixtures/verapdf/layout-clip-gradient.pdfa-2b.xml");
    let proven = out.receipt.clone().with_verapdf_report(&out.bytes, report).unwrap();
    assert_eq!(proven.conformance, hsk_studio_interop_layout::press::CONFORMANCE_PDFA2B_VERAPDF);
    assert_eq!(proven.conformance_report_sha256.as_deref(), Some("a1b6b1bc6310e608000aeec97225027e09384090295a48b32494f5fbb1a6a874"));
    let report = hsk_studio_interop_layout::press::check_pdfx4(&out.bytes, &Limits::default(), &cancel).unwrap();
    assert!(report.all_passed(), "{:?}", report.failed());
    assert_eq!(out.receipt.effective_pdfx_profile.as_deref(), Some("PDF/X-4"));
    let r = render_page(&out.bytes, 1, 1.0, &Limits::default(), &cancel).unwrap();
    let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 6);
    // Spread (395, 55) -> device (98, 64): inside both triangles, gradient t = 0.9.
    assert!(near(r.pixel(98, 64).unwrap(), [230; 3]), "{:?}", r.pixel(98, 64));
    // Spread (365, 52) -> device (68, 61): inside both, t = 0.3.
    assert!(near(r.pixel(68, 61).unwrap(), [77; 3]), "{:?}", r.pixel(68, 61));
    // Spread (355, 52) -> device (58, 61): inside vector 11 only, clipped away (page white).
    assert_eq!(r.pixel(58, 61), Some([255, 255, 255]));
    // Spread (405, 55) -> device (108, 64): the base alone (blue).
    assert!(near(r.pixel(108, 64).unwrap(), [0, 0, 255]), "{:?}", r.pixel(108, 64));
}

/// Item 3 through Folio (document order, no graph): vector 13 (black, page 1) is clipped to the
/// text layer 1 before it, whose glyphs are white, so black shows only inside the glyph outlines;
/// vector 14 (red, page 2) is clipped to raster 3 before it by the tile alpha (1, 0.5 | 0, 1).
#[test]
fn folio_clip_to_text_and_raster_bases() {
    let cancel = CancellationToken::default();
    let b = build();
    let place = placement(&b);
    let mut d = b.snapshot.document().clone();
    let clipped = || Some(LayerAppearance { opacity: 100.0, fill_opacity: 100.0, blend_mode: 2, clipped: true, isolation: Isolation::Isolated, mask: None });
    let mut v13 = layer(13, "vector", Payload::Vector { path_ids: vec![id("SVPT", 13)] }, None);
    v13.appearance = clipped();
    let mut v14 = layer(14, "vector", Payload::Vector { path_ids: vec![id("SVPT", 14)] }, None);
    v14.appearance = clipped();
    let (text, raster) = (d.layers[0].clone(), d.layers[2].clone());
    d.layers = vec![text, v13, raster, v14];
    let snapshot = validate_document(d, Budget::default(), &cancel, &Everything).unwrap();
    let mut design = StudioDesignRecords::new(id("SDOC", 1));
    for (n, rgb) in [(13u32, [0.0, 0.0, 0.0]), (14, [1.0, 0.0, 0.0])] {
        design.paint_styles.push(paint_style(n, StudioAppearanceRow::Fill {
            paint: StudioPaint::Solid { colour: StudioColor { r: rgb[0], g: rgb[1], b: rgb[2], colour_profile_id: ProfileBinding::Unassigned(()) } },
            opacity: 100.0,
            blend_mode: 2,
            visible: true,
        }));
        let mut node: StudioNodeDesign = serde_json::from_value(serde_json::json!({"node": {"node_kind": "layer", "node_id": id("SLYR", n)}})).unwrap();
        node.style_refs.insert("fill_style_id".into(), id("SSTY", n));
        design.nodes.push(node);
    }
    struct Squares(Assets);
    impl PdfAssets for Squares {
        fn path(&self, path_id: &str) -> Option<OwnedPath> {
            let x = if path_id == id("SVPT", 13) { 0.0 } else if path_id == id("SVPT", 14) { 300.0 } else { return None };
            Some(OwnedPath {
                elements: vec![
                    PathElement::MoveTo(PathPoint::new(x, 0.0)),
                    PathElement::LineTo(PathPoint::new(x + 300.0, 0.0)),
                    PathElement::LineTo(PathPoint::new(x + 300.0, 400.0)),
                    PathElement::LineTo(PathPoint::new(x, 400.0)),
                    PathElement::Close,
                ],
                fill_rule: NibRule::NonZero,
            })
        }
        fn raster(&self, layer_id: &str) -> Option<PlacedRaster<'_>> {
            self.0.raster(layer_id)
        }
        fn profile(&self, profile_id: &str) -> Option<&[u8]> {
            self.0.profile(profile_id)
        }
    }
    let assets = Squares(assets());
    let stories = [PdfStory { story: &b.story, text: TEXT, placement: &place, paragraphs: &b.paragraphs, glyphs: &b.glyphs, default_fill: Colour::DeviceGray(1.0) }];
    let fonts = [PdfFont { bytes: INTER, face_index: 0, hyphen: None }];
    let out = export_pdf(
        &PdfExportInput {
            snapshot: &snapshot,
            design: Some(&design),
            type_styles: None,
            stories: &stories,
            fonts: &fonts,
            assets: &assets,
            options: archival(),
            limits: Limits::default(),
        },
        &cancel,
    )
    .unwrap();
    dump("layout-clip-text-raster.pdf", &out.bytes);
    let codes: Vec<&str> = out.loss.iter().map(|l| l.code).collect();
    assert!(!codes.iter().any(|c| c.starts_with("clip_")), "{codes:?}");
    let p1 = render_page(&out.bytes, 0, 1.0, &Limits::default(), &cancel).unwrap();
    let ink = (43..60).flat_map(|y| (29..129).map(move |x| (x, y))).filter(|(x, y)| p1.pixel(*x, *y).is_some_and(|p| p[0] < 128)).count();
    assert!(ink > 50, "black inside the glyphs: {ink}");
    assert_eq!(p1.pixel(200, 300), Some([255, 255, 255]), "outside the text the clipped square is gone");
    let p2 = render_page(&out.bytes, 1, 1.0, &Limits::default(), &cancel).unwrap();
    let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 6);
    assert!(near(p2.pixel(108, 314).unwrap(), [255, 0, 0]), "alpha 1 {:?}", p2.pixel(108, 314));
    assert!(near(p2.pixel(118, 314).unwrap(), [238, 111, 111]), "alpha 0.5 {:?}", p2.pixel(118, 314));
    assert_eq!(p2.pixel(108, 324), Some([255, 255, 255]), "alpha 0");
    assert!(near(p2.pixel(118, 324).unwrap(), [255, 0, 0]), "alpha 1 over white {:?}", p2.pixel(118, 324));
    assert_eq!(p2.pixel(200, 200), Some([255, 255, 255]), "outside the tile");
}
