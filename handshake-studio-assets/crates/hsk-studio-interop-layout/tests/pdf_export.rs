//! Native document -> PDF (SMB-LAYOUT-PDF-EXPORT slice 2). A Folio snapshot built here (two-page
//! facing spread with bleed, a justified text frame, a filled and stroked vector layer, a raster
//! layer, a text layer whose story is not supplied) is laid out by hsk-studio-layout and lowered
//! by `export_pdf`. Oracles are hand-computed from Inter's advances and the Folio geometry and are
//! checked against the written bytes: press's strict reader for the page boxes and an independent
//! scan of the decoded content streams (miniz_oxide inflate + line tokens) for glyph positions,
//! paths and 16-bit samples.
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_folio::*;
use hsk_studio_interop_layout::layout::{
    Alignment, Composer, JustifyParams, LayoutGlyph, Limits as ReflowLimits, ReflowRequest, ShapedParagraph, SourceSpan,
    SpaceParams, Spacing, TypesetOptions, reflow_typeset,
};
use hsk_studio_interop_layout::press::{Colour, ExportOptions, Limits, PdfDoc};
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
/// `(gid, x, y)` of every `1 0 0 1 x y Tm` + `<gggg> Tj` pair of the content streams.
fn shown(streams: &[Vec<u8>]) -> Vec<(u16, f64, f64)> {
    let mut out = Vec::new();
    for s in streams {
        let text = String::from_utf8_lossy(s);
        let lines: Vec<&str> = text.lines().collect();
        for w in lines.windows(2) {
            let t: Vec<&str> = w[0].split(' ').collect();
            if t.len() == 7 && t[6] == "Tm" && w[1].ends_with(" Tj") && w[1].starts_with('<') {
                let gid = u16::from_str_radix(&w[1][1..5], 16).unwrap();
                out.push((gid, t[4].parse().unwrap(), t[5].parse().unwrap()));
            }
        }
    }
    out
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
        story_id: &b.story.story_id,
        text: TEXT,
        placement: &place,
        paragraphs: &b.paragraphs,
        glyphs: &b.glyphs,
        fill: Colour::DeviceGray(0.0),
    }];
    let fonts = [PdfFont { bytes: INTER, face_index: 0, hyphen: None }];
    let out = export_pdf(
        &PdfExportInput {
            snapshot: &b.snapshot,
            design: Some(&design),
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
    assert_eq!(codes, ["paint_colour_untagged", "story_not_supplied"], "{:?}", out.loss);
}

#[test]
fn misaligned_stale_and_refused_inputs() {
    let cancel = CancellationToken::default();
    let b = build();
    let place = placement(&b);
    let assets = assets();
    let fonts = [PdfFont { bytes: INTER, face_index: 0, hyphen: None }, PdfFont { bytes: b"not a font", face_index: 0, hyphen: None }];
    let run = |glyphs: &[Vec<PdfGlyph>], place: &hsk_studio_interop_layout::layout::LayoutProposal, token: &CancellationToken| {
        let stories = [PdfStory { story_id: &b.story.story_id, text: TEXT, placement: place, paragraphs: &b.paragraphs, glyphs, fill: Colour::DeviceGray(0.0) }];
        export_pdf(
            &PdfExportInput {
                snapshot: &b.snapshot,
                design: None,
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
}
