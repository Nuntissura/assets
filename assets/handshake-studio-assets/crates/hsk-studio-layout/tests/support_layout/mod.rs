//! Shared fixtures of the SMB-LAYOUT-UI tests. Toy font: every glyph advances 6 pt, line height
//! 12 pt, ascent 10 pt, fixed first baseline 10 pt, no insets. Pages are 200 x 200 pt with 10 pt
//! margins; a spread holds two pages (page 2 starts at spread x 200).
#![allow(dead_code)]
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_layout::*;

pub fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
pub fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
pub fn edges(v: f64) -> LayoutEdges {
    LayoutEdges { top: len(v), right: len(v), bottom: len(v), left: len(v) }
}
pub fn page(key: &str, side: PageSide) -> StudioPage {
    StudioPage { page_id: key.into(), width: len(200.0), height: len(200.0), side, margins: edges(10.0), bleed: None }
}
/// Frame geometry: page index, x, y, w, h, columns, gutter.
#[derive(Clone, Copy)]
pub struct Spec {
    pub page: usize,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub columns: u32,
    pub gutter: f64,
}
pub fn spec(page: usize, x: f64, y: f64, w: f64, h: f64) -> Spec {
    Spec { page, x, y, w, h, columns: 1, gutter: 0.0 }
}
pub fn on_spread() -> Parent {
    Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: id("SPGS", 1) })
}
/// Text frame layer `n` of story `story` linked to layers `previous` / `next`.
pub fn frame_layer(n: u32, story: u32, s: Spec, previous: Option<u32>, next: Option<u32>) -> StudioLayer {
    StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", n),
        name: format!("frame {n}"),
        parent: on_spread(),
        kind: "text".into(),
        payload: Payload::Text { story_id: id("STXT", story) },
        visible: true,
        appearance: None,
        text_frame: Some(StudioTextFrame {
            page_id: format!("page-{}", s.page + 1),
            bounds: Bounds { x: len(s.x), y: len(s.y), width: len(s.w), height: len(s.h) },
            inset_spacing: edges(0.0),
            text_column_count: s.columns,
            text_column_gutter: len(s.gutter),
            first_baseline_offset: FirstBaseline::Fixed,
            minimum_first_baseline_offset: len(10.0),
            vertical_justification: VerticalJustification::Top,
            auto_sizing_type: AutoSizing::Off,
            previous_text_frame: previous.map(|p| id("SLYR", p)),
            next_text_frame: next.map(|q| id("SLYR", q)),
        }),
    }
}
/// A raster layer `n` (anchored items in tests); `parent` defaults to the spread.
pub fn item_layer(n: u32, parent: Parent) -> StudioLayer {
    StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", n),
        name: format!("item {n}"),
        parent,
        kind: "raster".into(),
        payload: Payload::Raster { tiles: vec![] },
        visible: true,
        appearance: None,
        text_frame: None,
    }
}
pub fn document(layers: Vec<StudioLayer>, sides: [PageSide; 2]) -> StudioDocument {
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
            bounds: Bounds { x: len(0.0), y: len(0.0), width: len(400.0), height: len(200.0) },
            pages: vec![page("page-1", sides[0]), page("page-2", sides[1])],
        }],
        layers,
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    }
}
/// Story `n` of `text`; paragraphs are byte spans (story partitions include the newline, shaped
/// paragraphs exclude a trailing one).
pub fn story(n: u32, text: &str, paras: &[(u64, u64)]) -> (StudioTextStory, Vec<ShapedParagraph>) {
    let end = text.len() as u64;
    let st = StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", n),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.into() },
        paragraphs: paras
            .iter()
            .enumerate()
            .map(|(i, &(s, e))| StoryParagraph {
                start: s,
                end: paras.get(i + 1).map_or(end, |n| n.0).max(e),
                style_ref: None,
                paragraph_direction: None,
            })
            .collect(),
        character_ranges: vec![StoryRange { start: 0, end, style_ref: None, applied_language: None }],
    };
    (st, shape(text, paras))
}
pub fn shape(text: &str, paras: &[(u64, u64)]) -> Vec<ShapedParagraph> {
    paras
        .iter()
        .map(|&(s, e)| ShapedParagraph::from_fixed_advance(text, SourceSpan { start: s, end: e }, 6.0, 12.0, 10.0).unwrap())
        .collect()
}
pub fn records() -> StudioLayoutFlowRecords {
    StudioLayoutFlowRecords::new(id("SDOC", 1))
}
pub fn run(
    doc: &StudioDocument,
    st: &StudioTextStory,
    paras: &[ShapedParagraph],
    flow: &StudioLayoutFlowRecords,
) -> Result<LayoutProposal, LayoutError> {
    reflow_flow(
        &ReflowRequest {
            document: doc,
            story: st,
            resolved_text: None,
            paragraphs: paras,
            obstacles: &[],
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
pub fn text_of(st: &StudioTextStory) -> &str {
    match &st.content {
        StoryContent::Inline { text_utf8 } => text_utf8,
        StoryContent::Stored { .. } => panic!("stored"),
    }
}
